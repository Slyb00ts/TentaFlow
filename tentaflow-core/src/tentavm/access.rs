// ===== File: tentavm/access.rs — "Poproś administratora" (P00), the store ====
//
// One design decision drives every line here, so it goes first:
//
//     THE REQUEST ROW IS IMMUTABLE AND EACH DECISION IS ITS OWN ROW.
//
// The state of a request is not stored. It is a FUNCTION of the set of decision
// rows plus the clock:
//
//     rejected   if any decision rejects
//     approved   else if any decision approves
//     expired    else if the term has passed
//     pending    otherwise
//
// That is a join-semilattice, and every property this path needs falls out of
// it rather than being argued for separately:
//
//   * TWO ADMINISTRATORS DECIDING AT ONCE CONVERGE. Ala approves on node A,
//     Bogdan rejects on node B; both rows reach both nodes in whatever order,
//     both nodes fold `rejected`. With a mutable `state` column under LWW, A
//     would keep `approved` with a grant written and B `rejected` with none,
//     permanently — a split of the authorization itself.
//   * NO CLOCK DECIDES ANYTHING. "Earliest decision wins" and "highest HLC
//     wins" both read a timestamp a peer supplies, and the HLC skew ceiling is
//     an open problem (step 17): a node with a clock from the past would win
//     every decision it ever minted. Rejection-wins reads no clock at all.
//   * THE AUDIT CANNOT BE REWRITTEN. There is no `decided_by` on the request
//     row for a later write to overwrite; a decision that lost stays as its own
//     row, which is what an auditor reads.
//   * THE TEXT CANNOT CHANGE UNDER THE READER. `reason` and `expires_at` are
//     written once, so the window between "an administrator reads the inbox"
//     and "an administrator clicks Approve" has nothing in it to exploit.
//
// The grant an approval produces is COMPUTED here too (`project_grants`), never
// written by whoever clicked: a rejection arriving later has to be able to take
// it away, and it can only do that if the grant was a function of the set in
// the first place.

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};

use crate::db::DbPool;

/// How long a filed request stays decidable. The node picks it, not the
/// requester: somebody choosing their own term would choose forever, and the
/// term is what stops a request approved a month after everyone forgot it.
pub const REQUEST_TTL_SECS: i64 = 7 * 24 * 60 * 60;

fn digest32(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(parts.join("|").as_bytes());
    hex::encode(hasher.finalize())[..32].to_string()
}

/// The identity of a request is its CONTENT, so a peer cannot invent one.
///
/// `requested_at` is a component and a per-node attempt counter deliberately is
/// not: two nodes counting "this is attempt 1" would mint the SAME id for two
/// different re-requests, and retention pruning decided rows would make the
/// counter go down and collide with history that survived. A counter derived
/// from a set that something else prunes cannot be part of a primary key.
pub fn request_id(
    instance_id: &str,
    org_id: &str,
    requested_by: &str,
    scope: &str,
    host_id: Option<&str>,
    role: Option<&str>,
    requested_seq: &str,
) -> String {
    digest32(&[
        "vmar/v1",
        instance_id,
        org_id,
        requested_by,
        scope,
        host_id.unwrap_or(""),
        role.unwrap_or(""),
        requested_seq,
    ])
}

/// One monotonic, node-unique stamp per write — the sync HLC, rendered.
///
/// This is what W5 asks a key component to be: "a value independent of the
/// state the node sees". A per-node `attempt` counter is not (two nodes compute
/// the same one for two different re-requests, and retention pruning makes it go
/// down); a seconds-resolution timestamp is not either, as a second request in
/// the same second showed by colliding on the primary key.
pub fn next_seq() -> String {
    let hlc = crate::sync::runtime::core_hlc_now();
    format!("{}.{}.{}", hlc.wall_time_ms, hlc.logical, hlc.node_id)
}

/// The identity of a decision. `decided_seq` is the HLC stamp, which carries the
/// deciding node, so two administrators deciding in the same second on two
/// nodes cannot collide.
pub fn decision_id(request_id: &str, decided_by: &str, decided_seq: &str) -> String {
    digest32(&["vmad/v1", request_id, decided_by, decided_seq])
}

/// What an administrator is asked to bind their decision to (§15: "wiążą zgodę
/// z hashem planu pokazanego użytkownikowi").
///
/// The stored row is immutable, so this cannot catch a requester editing the
/// text — nothing can, there is no edit. What it catches is the other case: an
/// administrator deciding from a list their node had not yet replicated, where
/// the row they READ and the row they are deciding about differ.
pub fn content_digest(request: &StoredRequest) -> String {
    digest32(&[
        "vmarc/v1",
        &request.id,
        &request.scope,
        request.host_id.as_deref().unwrap_or(""),
        request.role.as_deref().unwrap_or(""),
        &request.reason,
        &request.requested_by,
        &request.expires_at,
    ])
}

/// One `vm_access_requests` row, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRequest {
    pub id: String,
    pub instance_id: String,
    pub org_id: String,
    pub scope: String,
    pub host_id: Option<String>,
    pub role: Option<String>,
    pub reason: String,
    pub requested_by: String,
    pub requested_at: String,
    pub expires_at: String,
}

/// One `vm_access_decisions` row, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDecision {
    pub decision: String,
    pub note: String,
    pub decided_by: String,
    pub decided_at: String,
}

/// The state of one request and the decision that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folded {
    pub state: String,
    pub decided_by: String,
    pub decided_at: String,
    pub decision_note: String,
}

/// The fold, and the only definition of what a request's state IS.
///
/// `now` is passed in rather than read here so the caller can fold "as of the
/// moment of a decision" — which is exactly what the expiry gate needs: a
/// decision is judged against the term as it stood when it was MADE, not when
/// it happened to be applied on some slow node.
pub fn fold(request: &StoredRequest, decisions: &[StoredDecision], now: &str) -> Folded {
    // Rejection wins, so scanning for one first is not an optimization — it is
    // the rule. Among several rejections (or several approvals) the winner is
    // the lexicographically smallest `decided_at`, which is a total order every
    // node computes from the same data; it decides only WHICH rejection is
    // shown, never whether the request is rejected.
    let pick = |kind: &str| -> Option<&StoredDecision> {
        decisions
            .iter()
            .filter(|d| d.decision == kind)
            .min_by(|a, b| {
                a.decided_at
                    .cmp(&b.decided_at)
                    .then_with(|| a.decided_by.cmp(&b.decided_by))
            })
    };
    let winner = pick("reject").or_else(|| pick("approve"));
    match winner {
        Some(decision) => Folded {
            state: if decision.decision == "reject" {
                "rejected".to_string()
            } else {
                "approved".to_string()
            },
            decided_by: decision.decided_by.clone(),
            decided_at: decision.decided_at.clone(),
            decision_note: decision.note.clone(),
        },
        None => Folded {
            // String comparison is the right one here: both sides are RFC 3339
            // UTC with seconds precision (`tentavm::now`), which sorts
            // lexicographically in the same order it sorts chronologically.
            state: if request.expires_at.as_str() <= now {
                "expired".to_string()
            } else {
                "pending".to_string()
            },
            decided_by: String::new(),
            decided_at: String::new(),
            decision_note: String::new(),
        },
    }
}

/// Reads one request and its decisions.
pub fn load(
    conn: &rusqlite::Connection,
    instance_id: &str,
    org_id: &str,
    request_id: &str,
) -> Result<Option<(StoredRequest, Vec<StoredDecision>)>> {
    let request = conn
        .query_row(
            "SELECT id, instance_id, org_id, scope, host_id, role, reason, requested_by, \
                    requested_at, expires_at \
             FROM vm_access_requests WHERE id = ?1 AND instance_id = ?2 AND org_id = ?3",
            rusqlite::params![request_id, instance_id, org_id],
            |row| {
                Ok(StoredRequest {
                    id: row.get(0)?,
                    instance_id: row.get(1)?,
                    org_id: row.get(2)?,
                    scope: row.get(3)?,
                    host_id: row.get(4)?,
                    role: row.get(5)?,
                    reason: row.get(6)?,
                    requested_by: row.get(7)?,
                    requested_at: row.get(8)?,
                    expires_at: row.get(9)?,
                })
            },
        )
        .ok();
    let Some(request) = request else {
        return Ok(None);
    };
    let decisions = decisions_of(conn, &request.id)?;
    Ok(Some((request, decisions)))
}

pub fn decisions_of(
    conn: &rusqlite::Connection,
    request_id: &str,
) -> Result<Vec<StoredDecision>> {
    let mut stmt = conn.prepare(
        "SELECT decision, note, decided_by, decided_at FROM vm_access_decisions \
         WHERE request_id = ?1",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![request_id], |row| {
            Ok(StoredDecision {
                decision: row.get(0)?,
                note: row.get(1)?,
                decided_by: row.get(2)?,
                decided_at: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Brings `vm_host_grants` into agreement with the decision set of ONE request,
/// inside the caller's transaction.
///
/// This is what W2 asks for and the reason it is not a side effect of a click:
/// the grant is a function of state, so the same code produces it on every node
/// and REMOVES it when a rejection arrives after an approval. A grant written by
/// a handler "while it was there" would live outside the convergence rule and a
/// losing approval could never be undone.
///
/// Rows it writes carry `source = 'access_request'` and the `request_id`, which
/// is what lets the H06 matrix refuse to edit them: a computed row that somebody
/// hand-edited would drift from its decision the moment the fold ran again.
///
/// Returns the grant key it touched, if any, so the caller can capture it.
pub fn project_grants(
    tx: &rusqlite::Transaction<'_>,
    request: &StoredRequest,
    decisions: &[StoredDecision],
    now: &str,
    local_node_id: &str,
) -> Result<Option<(String, String, String, String)>> {
    if request.scope != "host_role" {
        // An `instance_create` request grants a PERMISSION, not a host role, and
        // the permission matrix is not this registry. Nothing to project; the
        // approved request is what an administrator acts on.
        return Ok(None);
    }
    let (Some(host_id), Some(role)) = (request.host_id.as_deref(), request.role.as_deref()) else {
        return Err(anyhow!(
            "a host_role request without a host and a role reached the projection"
        ));
    };
    let folded = fold(request, decisions, now);
    let key = (
        request.instance_id.clone(),
        host_id.to_string(),
        "user".to_string(),
        request.requested_by.clone(),
    );
    if folded.state == "approved" {
        tx.execute(
            "INSERT INTO vm_host_grants \
                (instance_id, host_id, subject_kind, subject_id, org_id, role, granted_by, \
                 created_at, updated_at, updated_by_node, source, request_id) \
             VALUES (?1, ?2, 'user', ?3, ?4, ?5, ?6, ?7, ?7, ?8, 'access_request', ?9) \
             ON CONFLICT(instance_id, host_id, subject_kind, subject_id) DO UPDATE SET \
                 role = excluded.role, \
                 granted_by = excluded.granted_by, \
                 updated_at = excluded.updated_at, \
                 updated_by_node = excluded.updated_by_node, \
                 source = excluded.source, \
                 request_id = excluded.request_id \
             WHERE vm_host_grants.role <> excluded.role \
                OR vm_host_grants.source <> excluded.source \
                OR COALESCE(vm_host_grants.request_id, '') <> excluded.request_id",
            rusqlite::params![
                request.instance_id,
                host_id,
                request.requested_by,
                request.org_id,
                role,
                folded.decided_by,
                now,
                local_node_id,
                request.id,
            ],
        )?;
    } else {
        // Only a row THIS request produced is removed. A grant an administrator
        // typed into H06 for the same person and host is theirs, and a rejected
        // request must not take it away.
        tx.execute(
            "DELETE FROM vm_host_grants \
             WHERE instance_id = ?1 AND host_id = ?2 AND subject_kind = 'user' \
               AND subject_id = ?3 AND source = 'access_request' AND request_id = ?4",
            rusqlite::params![request.instance_id, host_id, request.requested_by, request.id],
        )?;
    }
    Ok(Some(key))
}

/// The caller's most recent request in this environment, folded — the value
/// `VmSummary.access_request` carries. "Most recent by `requested_at`", in
/// whatever state, because P00 has to draw the refusal and the expiry as well
/// as the wait.
pub fn latest_for_user(
    main_db: &DbPool,
    instance_id: &str,
    org_id: &str,
    user_id: &str,
    now: &str,
) -> Option<tentaflow_protocol::tentavm::VmAccessRequest> {
    let conn = main_db.read().ok()?;
    let request = conn
        .query_row(
            "SELECT id, instance_id, org_id, scope, host_id, role, reason, requested_by, \
                    requested_at, expires_at \
             FROM vm_access_requests \
             WHERE instance_id = ?1 AND org_id = ?2 AND requested_by = ?3 \
             ORDER BY requested_at DESC LIMIT 1",
            rusqlite::params![instance_id, org_id, user_id],
            |row| {
                Ok(StoredRequest {
                    id: row.get(0)?,
                    instance_id: row.get(1)?,
                    org_id: row.get(2)?,
                    scope: row.get(3)?,
                    host_id: row.get(4)?,
                    role: row.get(5)?,
                    reason: row.get(6)?,
                    requested_by: row.get(7)?,
                    requested_at: row.get(8)?,
                    expires_at: row.get(9)?,
                })
            },
        )
        .ok()?;
    let decisions = decisions_of(&conn, &request.id).ok()?;
    let label = subject_label(&conn, &request.requested_by);
    Some(to_wire(&request, &decisions, now, label))
}

/// The display name of a subject, or its id when the account is gone. A grant
/// and a request both outlive the account they name — the columns deliberately
/// have no foreign key — so an empty label would hide a live row.
pub fn subject_label(conn: &rusqlite::Connection, user_id: &str) -> String {
    conn.query_row(
        "SELECT COALESCE(NULLIF(display_name, ''), username) FROM user_accounts WHERE id = ?1",
        rusqlite::params![user_id],
        |row| row.get::<_, String>(0),
    )
    .unwrap_or_else(|_| user_id.to_string())
}

pub fn to_wire(
    request: &StoredRequest,
    decisions: &[StoredDecision],
    now: &str,
    requested_by_label: String,
) -> tentaflow_protocol::tentavm::VmAccessRequest {
    let folded = fold(request, decisions, now);
    tentaflow_protocol::tentavm::VmAccessRequest {
        request_id: request.id.clone(),
        scope: request.scope.clone(),
        host_id: request.host_id.clone(),
        role: request.role.clone().unwrap_or_default(),
        reason: request.reason.clone(),
        requested_by: request.requested_by.clone(),
        requested_by_label,
        requested_at: request.requested_at.clone(),
        expires_at: request.expires_at.clone(),
        state: folded.state,
        decided_by: folded.decided_by,
        decided_at: folded.decided_at,
        decision_note: folded.decision_note,
    }
}

/// Returned when a caller already has an open request for the same thing.
#[derive(Debug)]
pub struct AlreadyOpen;

impl std::fmt::Display for AlreadyOpen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an open request for this already exists")
    }
}

impl std::error::Error for AlreadyOpen {}

/// Files one request and captures it.
///
/// Idempotence is a check HERE and not a partial unique index, and the reason is
/// worth stating rather than hiding: "open" means "no decision row and the term
/// has not passed", which lives in a second table, and SQLite cannot index that.
/// So two nodes filing at the same instant produce two rows. That is harmless —
/// both are the same person asking for the same thing, both converge on every
/// node, and an administrator deciding either leaves the other to expire — and
/// it is better said out loud than promised by a constraint that cannot hold it.
#[allow(clippy::too_many_arguments)]
pub fn file(
    main_db: &DbPool,
    instance_id: &str,
    org_id: &str,
    requested_by: &str,
    scope: &str,
    host_id: Option<&str>,
    role: Option<&str>,
    reason: &str,
    local_node_id: &str,
) -> Result<String> {
    let now = super::now();
    let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(REQUEST_TTL_SECS))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut conn = main_db
        .write()
        .map_err(|e| anyhow!("tentavm access: main db lock: {e}"))?;
    let tx = conn.transaction()?;

    let open: Vec<StoredRequest> = {
        let mut stmt = tx.prepare(
            "SELECT id, instance_id, org_id, scope, host_id, role, reason, requested_by, \
                    requested_at, expires_at \
             FROM vm_access_requests \
             WHERE instance_id = ?1 AND org_id = ?2 AND requested_by = ?3 AND scope = ?4 \
               AND COALESCE(host_id, '') = ?5",
        )?;
        let rows = stmt
            .query_map(
                rusqlite::params![
                    instance_id,
                    org_id,
                    requested_by,
                    scope,
                    host_id.unwrap_or("")
                ],
                |row| {
                    Ok(StoredRequest {
                        id: row.get(0)?,
                        instance_id: row.get(1)?,
                        org_id: row.get(2)?,
                        scope: row.get(3)?,
                        host_id: row.get(4)?,
                        role: row.get(5)?,
                        reason: row.get(6)?,
                        requested_by: row.get(7)?,
                        requested_at: row.get(8)?,
                        expires_at: row.get(9)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for existing in &open {
        let decisions = decisions_of(&tx, &existing.id)?;
        if fold(existing, &decisions, &now).state == "pending" {
            return Err(anyhow::Error::new(AlreadyOpen));
        }
    }

    // `requested_at` is a key component, so the row's identity is fixed the
    // instant it is minted and nothing derived from local state enters it.
    let seq = next_seq();
    let id = request_id(instance_id, org_id, requested_by, scope, host_id, role, &seq);
    tx.execute(
        "INSERT INTO vm_access_requests \
            (id, instance_id, org_id, scope, host_id, role, reason, requested_by, \
             requested_at, requested_seq, expires_at, owner_node_id, created_at, updated_at, \
             updated_by_node) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?9, ?9, ?12)",
        rusqlite::params![
            id,
            instance_id,
            org_id,
            scope,
            host_id,
            role,
            reason,
            requested_by,
            now,
            seq,
            expires_at,
            local_node_id
        ],
    )?;
    crate::sync::tentavm_registry::capture_row(
        &tx,
        crate::sync::core_registry::CoreSyncResourceKind::VmAccessRequest,
        &[&id],
    )?;
    tx.commit()?;
    Ok(id)
}

/// Records one administrator's decision and re-projects the grant.
///
/// Both writes are in ONE transaction with both captures, because the grant is
/// a function of the decision set: a decision that committed without its
/// projection would leave this node holding a state its own fold disagrees with
/// until something else happened to re-run it.
pub fn decide(
    main_db: &DbPool,
    request: &StoredRequest,
    decided_by: &str,
    decision: &str,
    note: &str,
    local_node_id: &str,
) -> Result<String> {
    let now = super::now();
    let mut conn = main_db
        .write()
        .map_err(|e| anyhow!("tentavm access: main db lock: {e}"))?;
    let tx = conn.transaction()?;
    let seq = next_seq();
    let id = decision_id(&request.id, decided_by, &seq);
    tx.execute(
        "INSERT INTO vm_access_decisions \
            (id, request_id, instance_id, org_id, decision, note, decided_by, decided_at, \
             decided_seq, owner_node_id, created_at, updated_at, updated_by_node) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?8, ?8, ?10)",
        rusqlite::params![
            id,
            request.id,
            request.instance_id,
            request.org_id,
            decision,
            note,
            decided_by,
            now,
            seq,
            local_node_id
        ],
    )?;
    crate::sync::tentavm_registry::capture_row(
        &tx,
        crate::sync::core_registry::CoreSyncResourceKind::VmAccessDecision,
        &[&id],
    )?;
    reproject(&tx, request, &now, local_node_id)?;
    tx.commit()?;
    Ok(id)
}

/// Re-folds one request and brings its computed grant into agreement, capturing
/// the grant row if it moved. Called after a local decision AND after a decision
/// arrives from the mesh, so every node reaches the same grant set from the same
/// decision set.
pub fn reproject(
    tx: &rusqlite::Transaction<'_>,
    request: &StoredRequest,
    now: &str,
    local_node_id: &str,
) -> Result<()> {
    let decisions = decisions_of(tx, &request.id)?;
    if let Some((instance_id, host_id, subject_kind, subject_id)) =
        project_grants(tx, request, &decisions, now, local_node_id)?
    {
        if tx.changes() > 0 {
            crate::sync::tentavm_registry::capture_row(
                tx,
                crate::sync::core_registry::CoreSyncResourceKind::VmHostGrant,
                &[&instance_id, &host_id, &subject_kind, &subject_id],
            )?;
        }
    }
    Ok(())
}
