// =============================================================================
// File: tentanas/approvals.rs — the second pair of eyes (plan-02 §5.10).
//       A red-path operation (destroy pool, lift a snapshot's protection,
//       delete a share that holds data, overwrite the configuration) does not
//       run when it is requested: it is PARKED in `tentanas.db` with a TTL,
//       and a DIFFERENT admin holding `nas.admin` releases it. The author is
//       refused here, on the node, not merely hidden in the dashboard.
//
//       Everything but the execution itself lives in this module so the rules
//       are testable without a request context: `Actor` carries the six facts
//       the flow needs, `park`/`claim`/`reject`/`expire_due` are the whole
//       state machine, and `dispatch::tentanas` only replays the stored
//       request once `claim` hands it over.
//
//       Two invariants the rest of the app depends on:
//       - an approved operation executes EXACTLY once (the state change out of
//         'pending' is a conditional UPDATE, so a retry finds nothing to do);
//       - a parked operation that reaches its TTL closes as expired and is
//         never executed.
// =============================================================================

use anyhow::{anyhow, Result};
use tentaflow_protocol::tentanas::{NasApprovalSettings, NasPendingApproval, TentaNasPayload};

use super::db as store;
use crate::db::DbPool;

/// The operations that route through approval. Each is one arm of
/// `dispatch::tentanas::execute_approved`, and nothing else may be parked.
pub const OP_POOL_DESTROY: &str = "pool_destroy";
pub const OP_SNAPSHOT_RELEASE: &str = "snapshot_release";
pub const OP_SHARE_DELETE: &str = "share_delete";
/// Removing a block target. Same blast radius as a share with data behind it:
/// the client loses a raw disk mid-write, and the target is the only thing
/// that told it where the disk was. The zvol survives either way (§5.5).
pub const OP_TARGET_DELETE: &str = "target_delete";
pub const OP_CONFIG_IMPORT: &str = "config_import";
pub const OP_ELASTIC_CREATE: &str = "elastic_create";
pub const OP_ELASTIC_SYNC: &str = "elastic_sync";
pub const OP_ELASTIC_SCRUB: &str = "elastic_scrub";
/// Running the mover by hand. It moves files off the cache and then syncs
/// parity, so it is a red path for the same reason a sync is.
pub const OP_ELASTIC_MOVER: &str = "elastic_mover";
/// Rebuilding one data disk from parity. Red for the reason a restore is not:
/// it WRITES the named disk, overwriting whatever it currently holds with what
/// the parity checkpoint says it should hold.
pub const OP_ELASTIC_FIX: &str = "elastic_fix";
/// Adding a data disk to a live array. The disk is formatted before it joins,
/// so this destroys whatever was on it — the same blast radius as one disk of
/// a create.
pub const OP_ELASTIC_ADD_DISK: &str = "elastic_add_disk";
/// Undoing an unfinished add of a data disk. It ERASES the filesystem that
/// add gave the disk, so it carries the add's own blast radius, and it takes
/// a slot out of an array's journal.
pub const OP_ELASTIC_ADD_DISK_ABORT: &str = "elastic_add_disk_abort";
/// DORMANT: disk replacement is withdrawn (round 4), the dispatch handler
/// refuses the request before anything is parked, and this operation therefore
/// cannot appear in the queue — which is why the UI's label allowlist
/// (`approvals.js`) deliberately does not carry it.
///
/// Replacing one data disk and rebuilding the array onto it. The replacement
/// is FORMATTED and a whole disk is written from parity, so it carries the
/// blast radius of an add and of a repair at once — and it is the operation
/// whose truth an admin is least likely to know without being told: a rebuild
/// restores the disk to its state at the last sync.
pub const OP_ELASTIC_REPLACE_DISK: &str = "elastic_replace_disk";
/// Dissolving the array.
///
/// It joins the four-eyes list even though it is REVERSIBLE — the disks keep
/// their filesystems and the array import takes the array back — because what
/// is at stake is not the bytes: dissolving is what takes an array's shares
/// off every client at once, and it is the one operation that lets the addon be
/// uninstalled (`db::block_elastic_teardown` refuses while any array row
/// stands). One admin alone taking a fleet's storage offline, and unlocking the
/// uninstall that follows it, is exactly what the second pair of eyes is for.
/// The plan's list already carries `destroy pool` for a ZFS pool; this is the
/// same decision for the other kind of array.
pub const OP_ELASTIC_DESTROY: &str = "elastic_destroy";

/// ARMING one of an array's cadences (E2-10).
///
/// Its own operation rather than the per-kind ones above, because what is
/// approved here is not the run: approving `elastic_mover` and getting a saved
/// schedule instead of a mover would make the audit row say something that did
/// not happen. It is a red path all the same — a schedule is a cheaper door to
/// exactly the privileged work the per-kind operations guard, deferred and
/// repeating, so one admin alone must not be able to arm one.
pub const OP_ELASTIC_SCHEDULE: &str = "elastic_schedule";

/// How long a parked operation stays approvable when nobody configured it.
/// A day is long enough for a colleague in another timezone and short enough
/// that a forgotten request cannot be approved next month.
pub const DEFAULT_TTL_HOURS: u32 = 24;

/// The switch, in the instance's synced `addon_config` — per fleet (§5.10),
/// because `tentanas.db` is per node — and PER ORGANISATION under
/// `__nas_four_eyes/<org_id>`.
///
/// WHY per organisation: TentaNas is ONE instance per node shared by every
/// tenant, and `addon_config` is keyed by that instance alone. One key for
/// everybody let an admin of org B switch four eyes OFF for org A — the
/// second pair of eyes guarding A's Elastic destroy would then be whatever B
/// decided. The admins who can approve are already counted per organisation
/// (`approver_ids`), so the switch they are the second pair for is too.
const SETTINGS_KEY: &str = "__nas_four_eyes";

/// The permission an approver must hold, on top of the org Admin role.
const PERM_ADMIN: &str = "nas.admin";

/// Four eyes is on by default as soon as a second admin exists to be the
/// second pair — a single-admin fleet would only be locking itself out.
pub fn default_enabled(admin_count: u32) -> bool {
    admin_count >= 2
}

/// Everything the flow needs about the caller and the instance, gathered once
/// by the dispatcher. The permission checker travels with it because counting
/// the admins is a permission question, and this module must never guess an
/// answer the gate would give differently.
pub struct Actor<'a> {
    pub main_db: &'a DbPool,
    pub nas_db: &'a DbPool,
    pub checker: &'a crate::addon::permissions::PermissionChecker,
    pub org_id: &'a str,
    pub addon_id: &'a str,
    pub node_id: &'a str,
    pub user_id: &'a str,
}

/// Why a decision was refused. Every variant is a state the dashboard can
/// reach honestly, so each carries the message the admin sees.
#[derive(Debug, PartialEq, Eq)]
pub enum ApprovalError {
    NotFound,
    /// The author may never approve their own request (§5.10).
    OwnRequest,
    /// Already approved, rejected or expired — nothing left to decide.
    Closed(String),
    /// The TTL passed; the row is closed as expired by the same call.
    Expired,
}

/// Written as a REFUSAL CODE (`refusal:<code>`), not a sentence: this text is
/// the message of the `ProtocolError` the Tasks queue shows
/// (`dispatch::tentanas::approval_error`), and the screen words the code in
/// the reader's language (`errMessage`, format.js). The English sentences
/// this used to print were the only English in an otherwise Polish toast.
/// A status `Closed` carries that is not one of the three a request closes
/// with reads as the generic "already decided".
impl std::fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = match self {
            Self::NotFound => "approval_not_found",
            Self::OwnRequest => "approval_own_request",
            Self::Closed(status) => match status.as_str() {
                "approved" => "approval_already_approved",
                "rejected" => "approval_already_rejected",
                "expired" => "approval_already_expired",
                _ => "approval_closed",
            },
            Self::Expired => "approval_expired",
        };
        write!(f, "refusal:{code}")
    }
}

// ----- the fleet setting ---------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredSettings {
    enabled: bool,
    ttl_hours: u32,
}

/// The key one organisation's switch is saved under.
fn settings_key(org_id: &str) -> String {
    format!("{SETTINGS_KEY}/{org_id}")
}

fn stored(main_db: &DbPool, org_id: &str, addon_id: &str) -> Option<StoredSettings> {
    // The prefixed read strips the prefix from every key it returns, so asking
    // for the organisation's whole key back comes with an EMPTY remainder —
    // that row is the one, and any other is a longer key that merely starts
    // the same way (`org-a` must not read `org-ab`'s switch).
    //
    // No fallback to the instance-wide `__nas_four_eyes` row of earlier
    // builds: measured on rig11 (2026-09-23) no such row exists, and honouring
    // one would keep applying whichever tenant saved it to every other.
    crate::db::repository::list_addon_config_prefixed(main_db, addon_id, &settings_key(org_id))
        .ok()?
        .into_iter()
        .find(|(rest, _, _)| rest.is_empty())
        .and_then(|(_, value, _)| serde_json::from_str(&value).ok())
}

/// Admins who could approve: org Admins of this org that also hold
/// `nas.admin` on the instance — exactly the pair of checks `gate_admin`
/// makes, read from the live membership rather than from a stored number.
pub fn approver_ids(
    main_db: &DbPool,
    checker: &crate::addon::permissions::PermissionChecker,
    org_id: &str,
    addon_id: &str,
) -> Vec<String> {
    crate::services::org::repo::list_memberships_for_org(main_db, org_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, role)| role.permissions.iter().any(|p| p == "org.admin"))
        .filter(|(user_id, _)| {
            checker
                .check(addon_id, user_id, PERM_ADMIN, None)
                .is_granted()
        })
        .map(|(user_id, _)| user_id)
        .collect()
}

/// The switch as the settings card shows it: the saved choice, or the
/// ≥2-admin default when nobody saved one.
pub fn settings(
    main_db: &DbPool,
    checker: &crate::addon::permissions::PermissionChecker,
    org_id: &str,
    addon_id: &str,
) -> NasApprovalSettings {
    let admin_count = approver_ids(main_db, checker, org_id, addon_id).len() as u32;
    match stored(main_db, org_id, addon_id) {
        Some(s) => NasApprovalSettings {
            enabled: s.enabled,
            ttl_hours: if s.ttl_hours == 0 { DEFAULT_TTL_HOURS } else { s.ttl_hours },
            admin_count,
            by_default: false,
        },
        None => NasApprovalSettings {
            enabled: default_enabled(admin_count),
            ttl_hours: DEFAULT_TTL_HOURS,
            admin_count,
            by_default: true,
        },
    }
}

/// Saves the caller's organisation's switch. `ttl_hours` = 0 keeps whatever
/// is in effect, so the toggle does not have to resend a number the card
/// never showed.
pub fn set_settings(a: &Actor<'_>, enabled: bool, ttl_hours: u32) -> Result<NasApprovalSettings> {
    let current = settings(a.main_db, a.checker, a.org_id, a.addon_id);
    let value = serde_json::to_string(&StoredSettings {
        enabled,
        ttl_hours: if ttl_hours == 0 { current.ttl_hours } else { ttl_hours },
    })?;
    crate::db::repository::upsert_addon_config_value(
        a.main_db,
        a.addon_id,
        &settings_key(a.org_id),
        &value,
        false,
        Some(a.user_id),
    )?;
    Ok(settings(a.main_db, a.checker, a.org_id, a.addon_id))
}

// ----- parking and deciding -------------------------------------------------------

/// Whether a red path must park.
pub fn required(a: &Actor<'_>) -> bool {
    settings(a.main_db, a.checker, a.org_id, a.addon_id).enabled
}

/// Whether a SECOND pair of eyes exists on this fleet at all — two or more
/// admins who could approve, counted from the live membership.
///
/// This is not the four-eyes switch: lifting a snapshot protection ignores the
/// switch and parks whenever a second admin exists, because the approved
/// release is the only way a hold comes off. What it decides is the owner's
/// ruling of 2026-09-03: with FEWER than two admins there is nobody to be the
/// second pair, so the release runs as an ordinary red path (retype + sudo)
/// instead of a protected snapshot becoming unremovable forever and the pool
/// having no way out of the GUI. Server-side by construction — the count comes
/// from `approver_ids`, never from the client.
pub fn second_pair_available(a: &Actor<'_>) -> bool {
    default_enabled(approver_ids(a.main_db, a.checker, a.org_id, a.addon_id).len() as u32)
}

/// Parks `payload` and tells everyone: an audit row, and an alert through the
/// node's own pipeline (§5.9) so the pending operation reaches whoever is
/// watching alerts rather than only whoever opens the Tasks tab.
///
/// The payload is stored WITHOUT its sudo password — a password never reaches
/// disk (§3.4) — so the approver supplies their own when the operation runs.
pub fn park(
    a: &Actor<'_>,
    operation: &str,
    subject: &str,
    detail: &str,
    payload: &TentaNasPayload,
) -> Result<NasPendingApproval> {
    park_shown(a, operation, subject, subject, detail, payload)
}

/// `park` for a subject that is not its own name: `subject` is stored (the
/// request, the alert's `subject` parameter) and resolved at the read
/// boundary, while the alert's English title — forwarded to syslog and
/// webhooks as written — names `shown_subject`, or no subject when it is
/// empty. A config import from an export without a node name stores the
/// node's id; its title must never carry it (owner's rule: no ids).
pub fn park_shown(
    a: &Actor<'_>,
    operation: &str,
    subject: &str,
    shown_subject: &str,
    detail: &str,
    payload: &TentaNasPayload,
) -> Result<NasPendingApproval> {
    let ttl_hours = settings(a.main_db, a.checker, a.org_id, a.addon_id).ttl_hours;
    let now = chrono::Utc::now();
    let approval = NasPendingApproval {
        request_id: uuid::Uuid::now_v7().to_string(),
        operation: operation.to_string(),
        subject: subject.to_string(),
        detail: detail.to_string(),
        status: "pending".to_string(),
        requested_by: a.user_id.to_string(),
        requested_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        expires_at: (now + chrono::Duration::hours(i64::from(ttl_hours)))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        decided_by: None,
        decided_at: None,
        decision_note: String::new(),
        decision_job_id: None,
        is_own_request: true,
    };
    let row = store::ApprovalRow {
        payload_json: serde_json::to_string(&without_secret(payload))?,
        org_id: a.org_id.to_string(),
        addon_id: a.addon_id.to_string(),
        approval: approval.clone(),
    };
    store::insert_approval(a.nas_db, &row)?;
    let _ = store::raise_coded_alert(
        a.nas_db,
        &alert_key(&approval.request_id),
        "warning",
        "approval",
        &approval.request_id,
        &approval_alert_text(operation, subject, shown_subject, detail),
    );
    audit_as(a, "nas.approval.requested", &approval, "pending");
    Ok(approval)
}

/// Takes one pending operation for execution. Returns the row (with the
/// request to replay) only when this caller is allowed to release it and the
/// row was still pending — which is what makes the execution happen once.
pub fn claim(a: &Actor<'_>, request_id: &str) -> Result<store::ApprovalRow, ApprovalError> {
    let mut row = load(a, request_id)?;
    if row.approval.requested_by == a.user_id {
        return Err(ApprovalError::OwnRequest);
    }
    if !store::close_approval(a.nas_db, request_id, "approved", Some(a.user_id), "")
        .unwrap_or(false)
    {
        // Somebody decided between the load and the update.
        let status = load(a, request_id)
            .map(|r| r.approval.status)
            .unwrap_or_else(|_| "closed".to_string());
        return Err(ApprovalError::Closed(status));
    }
    row.approval.status = "approved".to_string();
    row.approval.decided_by = Some(a.user_id.to_string());
    let _ = store::resolve_alert(a.nas_db, &alert_key(request_id));
    audit_as(a, "nas.approval.approved", &row.approval, "approved");
    Ok(row)
}

/// Closes one pending operation without running it.
pub fn reject(
    a: &Actor<'_>,
    request_id: &str,
    note: &str,
) -> Result<NasPendingApproval, ApprovalError> {
    let mut row = load(a, request_id)?;
    if row.approval.requested_by == a.user_id {
        return Err(ApprovalError::OwnRequest);
    }
    if !store::close_approval(a.nas_db, request_id, "rejected", Some(a.user_id), note)
        .unwrap_or(false)
    {
        // Somebody decided between the load and the update; report where the
        // row actually ended up, not where it was a moment ago.
        return Err(ApprovalError::Closed(
            store::approval(a.nas_db, request_id)
                .ok()
                .flatten()
                .map(|r| r.approval.status)
                .unwrap_or_else(|| "closed".to_string()),
        ));
    }
    row.approval.status = "rejected".to_string();
    row.approval.decided_by = Some(a.user_id.to_string());
    row.approval.decision_note = note.to_string();
    let _ = store::resolve_alert(a.nas_db, &alert_key(request_id));
    audit_as(a, "nas.approval.rejected", &row.approval, "rejected");
    Ok(row.approval)
}

/// Records what the approved operation started. A job id makes the row point
/// at its own log; a failure to start leaves the operation closed as 'failed'
/// rather than pretending it ran.
pub fn finish(a: &Actor<'_>, request_id: &str, job_id: Option<&str>) {
    let status = if job_id.is_some() { "approved" } else { "failed" };
    if let Err(e) = store::set_approval_outcome(a.nas_db, request_id, status, job_id) {
        tracing::warn!("tentanas: approval outcome not recorded: {e}");
    }
}

/// Closes every parked operation whose TTL has passed. Runs from the schedule
/// loop and from every list read, so an expired operation is expired whether
/// or not anybody is looking — and it is never executed afterwards, because
/// `claim` only accepts a row that is still 'pending'.
pub fn expire_due(main_db: &DbPool, nas_db: &DbPool, node_id: &str) -> Vec<NasPendingApproval> {
    let now = store::now();
    let due = store::approvals_past_ttl(nas_db, &now).unwrap_or_default();
    let mut closed = Vec::new();
    for mut row in due {
        if !store::close_approval(nas_db, &row.approval.request_id, "expired", None, "")
            .unwrap_or(false)
        {
            continue;
        }
        row.approval.status = "expired".to_string();
        let _ = store::resolve_alert(nas_db, &alert_key(&row.approval.request_id));
        // The author is the only user this row names; the expiry itself has no
        // actor, so the audit row records the operation, not a decision.
        audit(
            main_db,
            &row.org_id,
            &row.addon_id,
            node_id,
            &row.approval.requested_by,
            "nas.approval.expired",
            &row.approval,
            "expired",
        );
        closed.push(row.approval);
    }
    closed
}

/// Whether `row` is a request of the caller's organisation — the ONLY
/// requests a caller may see or decide.
///
/// WHY: `tentanas.db` is one per node and shared by every tenant on it, so
/// `nas_pending_approvals` holds every organisation's parked requests. Without
/// this an admin of org B saw org A's queue (array names, author) and could
/// APPROVE org A's parked Elastic destroy — replayed, moreover, under B's own
/// request context. An empty caller org matches nothing, so a request with no
/// real tenant can never see rows whose owner was recorded as empty.
fn is_callers(a: &Actor<'_>, row: &store::ApprovalRow) -> bool {
    !a.org_id.is_empty() && row.org_id == a.org_id
}

/// The list the Tasks tab shows — the caller's organisation's requests only —
/// with `is_own_request` resolved for the caller so the dashboard can grey out
/// its own approve button; the node refuses it either way.
pub fn list(a: &Actor<'_>, include_closed: bool) -> Result<Vec<NasPendingApproval>> {
    expire_due(a.main_db, a.nas_db, a.node_id);
    Ok(store::list_approvals(a.nas_db, &a.org_id, include_closed)
        .map_err(|e| anyhow!("{e}"))?
        .into_iter()
        .filter(|row| is_callers(a, row))
        .map(|row| NasPendingApproval {
            is_own_request: row.approval.requested_by == a.user_id,
            ..row.approval
        })
        .collect())
}

/// The stored request, decoded. A row whose payload no longer parses belongs
/// to a build that spoke a different wire — refusing to execute it is the only
/// safe answer for a red path.
pub fn stored_payload(row: &store::ApprovalRow) -> Result<TentaNasPayload> {
    serde_json::from_str(&row.payload_json)
        .map_err(|e| anyhow!("the parked request cannot be read back: {e}"))
}

/// The caller's pending request `request_id`.
///
/// Another organisation's request is `NotFound` — the very answer an id that
/// does not exist gets — and that check comes BEFORE the status and the TTL:
/// "already approved" or "expired" about a row the caller may not see would
/// confirm it exists and leak how it ended. Nor does it run the expiry sweep
/// on the caller's behalf, so the answer takes the same path as a missing id.
fn load(a: &Actor<'_>, request_id: &str) -> Result<store::ApprovalRow, ApprovalError> {
    let row = store::approval(a.nas_db, request_id)
        .ok()
        .flatten()
        .filter(|row| is_callers(a, row))
        .ok_or(ApprovalError::NotFound)?;
    if row.approval.status != "pending" {
        return Err(ApprovalError::Closed(row.approval.status));
    }
    if row.approval.expires_at <= store::now() {
        expire_due(a.main_db, a.nas_db, a.node_id);
        return Err(ApprovalError::Expired);
    }
    Ok(row)
}

/// The parked request's alert (code 'approval_pending'): the operation code
/// and the subject are what a screen words ("Zniszczenie puli 'tank' czeka
/// na drugiego administratora"); the English title and the request's own
/// detail stay for forwarding and the tooltip.
fn approval_alert_text(operation: &str, subject: &str, shown_subject: &str, detail: &str) -> store::AlertText {
    store::AlertText::new("approval_pending", approval_alert_title(shown_subject), detail)
        .param("operation", operation)
        .param("subject", subject)
}

/// The English title of a parked request's alert, naming `shown_subject` —
/// or, when it is empty, the operation alone. Also what the read boundary
/// rewrites a config import's stored title to once it has resolved the node
/// (`dispatch::tentanas::alerts_list`).
pub fn approval_alert_title(shown_subject: &str) -> String {
    if shown_subject.trim().is_empty() {
        "a red-path operation waits for a second admin".to_string()
    } else {
        format!("a red-path operation on '{shown_subject}' waits for a second admin")
    }
}

fn alert_key(request_id: &str) -> String {
    format!("approval:{request_id}")
}

/// The sudo password of the parked request, dropped. `SudoSecret` is
/// `Zeroizing` in memory and must never reach the database; the approver's own
/// password runs the operation.
fn without_secret(payload: &TentaNasPayload) -> TentaNasPayload {
    use TentaNasPayload as P;
    match payload.clone() {
        // The acknowledgement is the AUTHOR's decision and is kept: it is what
        // the approver releases.
        P::ElasticArraySyncRequest { name, acknowledge_parity_fault, .. } =>
            P::ElasticArraySyncRequest { name, acknowledge_parity_fault, sudo_password: None },
        P::ElasticArrayScrubRequest { name, .. } => P::ElasticArrayScrubRequest { name, sudo_password: None },
        P::ElasticArrayFixRequest { name, disk, confirm_disk, .. } =>
            P::ElasticArrayFixRequest { name, disk, confirm_disk, sudo_password: None },
        P::ElasticArrayAddDiskRequest { name, disk_id, confirm_name, .. } =>
            P::ElasticArrayAddDiskRequest { name, disk_id, confirm_name, sudo_password: None },
        P::ElasticArrayAddDiskAbortRequest { name, disk_id, confirm_name, .. } =>
            P::ElasticArrayAddDiskAbortRequest { name, disk_id, confirm_name, sudo_password: None },
        P::ElasticArrayDestroyRequest { name, confirm_name, .. } =>
            P::ElasticArrayDestroyRequest { name, confirm_name, sudo_password: None },
        P::ElasticArrayCreateRequest { name,filesystem,data_disk_ids,parity_disk_ids,cache_disk_ids,confirm_name,.. } =>
            P::ElasticArrayCreateRequest { name,filesystem,data_disk_ids,parity_disk_ids,cache_disk_ids,confirm_name,sudo_password:None },
        P::PoolDestroyRequest {
            name, confirm_name, ..
        } => P::PoolDestroyRequest {
            name,
            confirm_name,
            sudo_password: None,
        },
        P::ShareDeleteRequest {
            share_id,
            confirm_name,
            ..
        } => P::ShareDeleteRequest {
            share_id,
            confirm_name,
            sudo_password: None,
        },
        P::ConfigImportApplyRequest { json, .. } => P::ConfigImportApplyRequest {
            json,
            sudo_password: None,
        },
        // The release also carries a password since the single-admin red path
        // of 2026-09-03 exists; a parked one never uses it, and a password
        // must not reach disk even by a caller's mistake.
        P::SnapshotProtectionReleaseRequest {
            snapshot, reason, ..
        } => P::SnapshotProtectionReleaseRequest {
            snapshot,
            reason,
            confirm_snapshot: String::new(),
            sudo_password: None,
        },
        other => other,
    }
}

/// Both sides of the four-eyes flow leave a hash-chained audit row: who asked,
/// who decided, on what, and how it ended (§5.10 "audyt po obu stronach").
/// Takes the instance facts loose rather than an `Actor` so the expiry sweep,
/// which runs without a caller, writes the same row shape.
fn audit(
    main_db: &DbPool,
    org_id: &str,
    addon_id: &str,
    node_id: &str,
    user_id: &str,
    action: &str,
    approval: &NasPendingApproval,
    result: &str,
) {
    let details = serde_json::json!({
        "operation": approval.operation,
        "subject": approval.subject,
        "requested_by": approval.requested_by,
        "decided_by": approval.decided_by,
        "expires_at": approval.expires_at,
    })
    .to_string();
    let _ = crate::db::repository::log_audit_full(
        main_db,
        Some(user_id),
        Some(addon_id),
        action,
        Some("nas_approval"),
        Some(&approval.request_id),
        Some(&details),
        "warning",
        "A",
        Some(result),
        Some(org_id),
        None,
        Some(node_id),
    );
}

fn audit_as(a: &Actor<'_>, action: &str, approval: &NasPendingApproval, result: &str) {
    audit(
        a.main_db,
        a.org_id,
        a.addon_id,
        a.node_id,
        a.user_id,
        action,
        approval,
        result,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn nas_pool() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        store::migrate(&conn).expect("migrate");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    /// A real platform database: the ≥2-admin rule is read from actual
    /// memberships and grants, so the test builds actual memberships.
    fn main_pool() -> (TempDir, DbPool) {
        let dir = TempDir::new().expect("tempdir");
        let pool = crate::db::init(&dir.path().join("approvals_test.db")).expect("init");
        (dir, pool)
    }

    fn role_id(pool: &DbPool, name: &str) -> String {
        crate::services::org::repo::list_roles(pool)
            .expect("roles")
            .into_iter()
            .find(|r| r.name == name)
            .expect("role")
            .role_id
    }

    /// Registers the instance and grants `nas.admin` to `users`, then builds
    /// the checker the flow reads from that very database.
    fn install_instance(
        pool: &DbPool,
        addon_id: &str,
        users: &[&str],
    ) -> crate::addon::permissions::PermissionChecker {
        {
            let conn = pool.write().expect("write");
            conn.execute(
                "INSERT OR IGNORE INTO addons \
                 (addon_id, name, version, package_id, package_version, runtime, is_enabled) \
                 VALUES (?1, 'tentanas', '1.0.0', 'tentanas', '1.0.0', 'native', 1)",
                rusqlite::params![addon_id],
            )
            .expect("instance row");
        }
        for user in users {
            crate::db::repository::upsert_permission(
                pool, addon_id, "user", user, PERM_ADMIN, "allow", None,
            )
            .expect("grant");
        }
        let checker = crate::addon::permissions::PermissionChecker::new(pool.clone());
        checker.refresh_all();
        checker
    }

    struct Fixture {
        _dir: TempDir,
        main: DbPool,
        nas: DbPool,
        checker: crate::addon::permissions::PermissionChecker,
        org_id: String,
        addon_id: String,
    }

    impl Fixture {
        fn actor<'a>(&'a self, user_id: &'a str) -> Actor<'a> {
            Actor {
                main_db: &self.main,
                nas_db: &self.nas,
                checker: &self.checker,
                org_id: &self.org_id,
                addon_id: &self.addon_id,
                node_id: "node-test",
                user_id,
            }
        }

        fn settings(&self) -> NasApprovalSettings {
            settings(&self.main, &self.checker, &self.org_id, &self.addon_id)
        }

        /// The same instance and node, asked by a member of `org_id`.
        fn actor_in<'a>(&'a self, org_id: &'a str, user_id: &'a str) -> Actor<'a> {
            Actor { org_id, ..self.actor(user_id) }
        }

        /// A SECOND tenant on the same node: its own organisation, whose
        /// `admins` are org Admins holding `nas.admin` on the one shared
        /// instance — exactly what makes them approvers in their own org.
        fn second_org(&self, admins: &[&str]) -> String {
            let org = crate::services::org::repo::create_organization(
                &self.main, "Beta", "beta", None, None, None, None,
            )
            .expect("org");
            let admin_role = role_id(&self.main, "org_admin");
            for user in admins {
                crate::services::org::repo::add_membership(
                    &self.main, &org.org_id, user, &admin_role, "test",
                )
                .expect("membership");
                crate::db::repository::upsert_permission(
                    &self.main, &self.addon_id, "user", user, PERM_ADMIN, "allow", None,
                )
                .expect("grant");
            }
            self.checker.refresh_all();
            org.org_id
        }
    }

    /// One org with `admins` as org Admins holding `nas.admin` and `members`
    /// as plain members (org_viewer) holding the same grant — the grant alone
    /// must not make an approver.
    fn fixture(admins: &[&str], members: &[&str]) -> Fixture {
        let (dir, main) = main_pool();
        let org = crate::services::org::repo::create_organization(
            &main, "Acme", "acme", None, None, None, None,
        )
        .expect("org");
        let admin_role = role_id(&main, "org_admin");
        let member_role = role_id(&main, "org_viewer");
        for user in admins {
            crate::services::org::repo::add_membership(&main, &org.org_id, user, &admin_role, "test")
                .expect("membership");
        }
        for user in members {
            crate::services::org::repo::add_membership(
                &main,
                &org.org_id,
                user,
                &member_role,
                "test",
            )
            .expect("membership");
        }
        let addon_id = format!("tentanas-{}", uuid::Uuid::now_v7());
        let everyone: Vec<&str> = admins.iter().chain(members.iter()).copied().collect();
        let checker = install_instance(&main, &addon_id, &everyone);
        Fixture {
            _dir: dir,
            main,
            nas: nas_pool(),
            checker,
            org_id: org.org_id,
            addon_id,
        }
    }

    fn destroy_request() -> TentaNasPayload {
        TentaNasPayload::PoolDestroyRequest {
            name: "tank".to_string(),
            confirm_name: "tank".to_string(),
            sudo_password: Some(tentaflow_protocol::tentanas::SudoSecret(
                "hunter2".to_string(),
            )),
        }
    }

    fn park_destroy(f: &Fixture, author: &str) -> NasPendingApproval {
        park(
            &f.actor(author),
            OP_POOL_DESTROY,
            "tank",
            "niszczy pulę tank",
            &destroy_request(),
        )
        .expect("park")
    }

    fn audit_actions(main: &DbPool) -> Vec<(String, String, String)> {
        let conn = main.read().expect("read");
        let mut stmt = conn
            .prepare(
                "SELECT action, user_id, result FROM audit_log \
                 WHERE action LIKE 'nas.approval.%' ORDER BY id",
            )
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("rows")
    }

    #[test]
    fn four_eyes_defaults_to_on_only_once_a_second_admin_exists() {
        assert!(!default_enabled(0));
        assert!(!default_enabled(1));
        assert!(default_enabled(2));

        // One admin plus a plain member: the member cannot approve, so the
        // fleet has one pair of eyes and the default stays off.
        let one = fixture(&["u-anna"], &["u-jan"]);
        let s = one.settings();
        assert_eq!(s.admin_count, 1, "only org Admins with nas.admin count");
        assert!(!s.enabled);
        assert!(s.by_default);
        assert_eq!(s.ttl_hours, DEFAULT_TTL_HOURS);

        let two = fixture(&["u-anna", "u-piotr"], &["u-jan"]);
        let s = two.settings();
        assert_eq!(s.admin_count, 2);
        assert!(s.enabled && s.by_default);
        assert_eq!(
            approver_ids(&two.main, &two.checker, &two.org_id, &two.addon_id).len(),
            2
        );

        // A saved choice wins over the default in both directions.
        let saved = set_settings(&two.actor("u-anna"), false, 6).expect("save");
        assert!(!saved.enabled && !saved.by_default && saved.ttl_hours == 6);
        assert!(!required(&two.actor("u-anna")));
        // ttl_hours = 0 keeps the saved TTL instead of resetting it.
        let saved = set_settings(&two.actor("u-anna"), true, 0).expect("save");
        assert!(saved.enabled && saved.ttl_hours == 6);
        assert!(required(&two.actor("u-anna")));
    }

    #[test]
    fn the_author_of_a_request_can_never_approve_it() {
        let f = fixture(&["u-anna", "u-piotr"], &[]);
        let parked = park_destroy(&f, "u-anna");
        assert!(parked.is_own_request);

        assert_eq!(
            claim(&f.actor("u-anna"), &parked.request_id).unwrap_err(),
            ApprovalError::OwnRequest
        );
        assert_eq!(
            reject(&f.actor("u-anna"), &parked.request_id, "nie").unwrap_err(),
            ApprovalError::OwnRequest
        );
        // The refusal changed nothing: the operation is still waiting.
        let open = list(&f.actor("u-anna"), false).expect("list");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].status, "pending");
        assert!(open[0].is_own_request);
        // …and it does not look like the approver's own request.
        assert!(!list(&f.actor("u-piotr"), false).expect("list")[0].is_own_request);
    }

    #[test]
    fn a_second_admin_releases_the_operation_exactly_once() {
        let f = fixture(&["u-anna", "u-piotr"], &[]);
        let parked = park_destroy(&f, "u-anna");

        let claimed = claim(&f.actor("u-piotr"), &parked.request_id).expect("claim");
        assert_eq!(claimed.approval.decided_by.as_deref(), Some("u-piotr"));
        // The stored request is the one to replay — without the author's
        // password, which never reached the database.
        match stored_payload(&claimed).expect("payload") {
            TentaNasPayload::PoolDestroyRequest {
                name,
                confirm_name,
                sudo_password,
            } => {
                assert_eq!((name.as_str(), confirm_name.as_str()), ("tank", "tank"));
                assert!(sudo_password.is_none(), "no password on disk");
            }
            other => panic!("{other:?}"),
        }
        assert!(!claimed.payload_json.contains("hunter2"));

        // Every parkable request is stripped the same way, including the
        // release, which grew a password with the single-admin red path.
        for payload in [
            destroy_request(),
            TentaNasPayload::ElasticArraySyncRequest {
                name: "media".into(),
                acknowledge_parity_fault: Some("018f2c1e-6b9a-7c3d-8e4f-5a6b7c8d9e0f".into()),
                sudo_password: Some(tentaflow_protocol::tentanas::SudoSecret("hunter2".into())),
            },
            TentaNasPayload::ElasticArrayAddDiskAbortRequest {
                name: "media".into(),
                disk_id: "wwn-0x5000c500a1b2c3d4".into(),
                confirm_name: "media".into(),
                sudo_password: Some(tentaflow_protocol::tentanas::SudoSecret("hunter2".into())),
            },
            TentaNasPayload::ElasticArrayScrubRequest {
                name: "media".into(),
                sudo_password: Some(tentaflow_protocol::tentanas::SudoSecret("hunter2".into())),
            },
            TentaNasPayload::ElasticArrayCreateRequest {
                name: "media".into(),
                filesystem: "xfs".into(),
                data_disk_ids: vec!["data".into()],
                parity_disk_ids: Vec::new(),
                cache_disk_ids: vec!["cache".into()],
                confirm_name: "media".into(),
                sudo_password: Some(tentaflow_protocol::tentanas::SudoSecret("hunter2".into())),
            },
            TentaNasPayload::SnapshotProtectionReleaseRequest {
                snapshot: "tank/p@przed-migracja".to_string(),
                reason: "koniec projektu".to_string(),
                confirm_snapshot: "tank/p@przed-migracja".to_string(),
                sudo_password: Some(tentaflow_protocol::tentanas::SudoSecret(
                    "hunter2".to_string(),
                )),
            },
            TentaNasPayload::ShareDeleteRequest {
                share_id: "s1".to_string(),
                confirm_name: "projekty".to_string(),
                sudo_password: Some(tentaflow_protocol::tentanas::SudoSecret(
                    "hunter2".to_string(),
                )),
            },
            TentaNasPayload::ConfigImportApplyRequest {
                json: "{}".to_string(),
                sudo_password: Some(tentaflow_protocol::tentanas::SudoSecret(
                    "hunter2".to_string(),
                )),
            },
        ] {
            let stored = serde_json::to_string(&without_secret(&payload)).expect("encode");
            assert!(!stored.contains("hunter2"), "{stored}");
        }
        let cache_payload = TentaNasPayload::ElasticArrayCreateRequest {
            name: "media".into(), filesystem: "xfs".into(), data_disk_ids: vec!["data".into()],
            parity_disk_ids: Vec::new(), cache_disk_ids: vec!["cache".into()],
            confirm_name: "media".into(),
            sudo_password: Some(tentaflow_protocol::tentanas::SudoSecret("hunter2".into())),
        };
        // The acknowledgement a Sync was parked with is what the approver
        // releases: stripping the password must not strip the decision.
        let acknowledged = without_secret(&TentaNasPayload::ElasticArraySyncRequest {
            name: "media".into(),
            acknowledge_parity_fault: Some("018f2c1e-6b9a-7c3d-8e4f-5a6b7c8d9e0f".into()),
            sudo_password: Some(tentaflow_protocol::tentanas::SudoSecret("hunter2".into())),
        });
        assert!(matches!(
            acknowledged,
            TentaNasPayload::ElasticArraySyncRequest { acknowledge_parity_fault: Some(_), sudo_password: None, .. }
        ));
        let redacted = without_secret(&cache_payload);
        if let TentaNasPayload::ElasticArrayCreateRequest { cache_disk_ids, sudo_password, .. } = redacted {
            assert_eq!(cache_disk_ids, vec!["cache".to_string()]);
            assert!(sudo_password.is_none());
        } else {
            panic!("oczekiwano zanonimizowanego Create");
        }

        finish(&f.actor("u-piotr"), &parked.request_id, Some("job-1"));

        // A second claim — a retry, or a second admin in another tab — finds
        // nothing to execute.
        assert_eq!(
            claim(&f.actor("u-piotr"), &parked.request_id).unwrap_err(),
            ApprovalError::Closed("approved".to_string())
        );
        let all = list(&f.actor("u-anna"), true).expect("list");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, "approved");
        assert_eq!(all[0].decision_job_id.as_deref(), Some("job-1"));
        assert!(list(&f.actor("u-anna"), false).expect("list").is_empty());

        // Both sides are audited, and the two rows name two different admins.
        let rows = audit_actions(&f.main);
        assert_eq!(
            rows,
            vec![
                (
                    "nas.approval.requested".to_string(),
                    "u-anna".to_string(),
                    "pending".to_string()
                ),
                (
                    "nas.approval.approved".to_string(),
                    "u-piotr".to_string(),
                    "approved".to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_rejected_operation_is_closed_and_audited_without_running() {
        let f = fixture(&["u-anna", "u-piotr"], &[]);
        let parked = park_destroy(&f, "u-anna");
        let rejected =
            reject(&f.actor("u-piotr"), &parked.request_id, "pula jest w użyciu").expect("reject");
        assert_eq!(rejected.status, "rejected");
        assert_eq!(rejected.decision_note, "pula jest w użyciu");
        assert_eq!(
            claim(&f.actor("u-piotr"), &parked.request_id).unwrap_err(),
            ApprovalError::Closed("rejected".to_string())
        );
        let rows = audit_actions(&f.main);
        assert_eq!(rows[1].0, "nas.approval.rejected");
        assert_eq!(rows[1].2, "rejected");
    }

    #[test]
    fn an_operation_that_reaches_its_ttl_is_closed_expired_and_never_executed() {
        let f = fixture(&["u-anna", "u-piotr"], &[]);
        let parked = park_destroy(&f, "u-anna");
        // Move the deadline into the past, the way a day of waiting would.
        {
            let conn = f.nas.write().expect("write");
            conn.execute(
                "UPDATE nas_pending_approvals SET expires_at = ?2 WHERE request_id = ?1",
                rusqlite::params![parked.request_id, "2020-01-01T00:00:00Z"],
            )
            .expect("expire");
        }
        // Reading the list is enough to close it.
        let open = list(&f.actor("u-piotr"), false).expect("list");
        assert!(open.is_empty());
        let all = list(&f.actor("u-piotr"), true).expect("list");
        assert_eq!(all[0].status, "expired");
        assert_eq!(all[0].decided_by, None, "nobody decided — the clock did");

        // And the approval is refused afterwards, so nothing executes late.
        assert_eq!(
            claim(&f.actor("u-piotr"), &parked.request_id).unwrap_err(),
            ApprovalError::Closed("expired".to_string())
        );
        let rows = audit_actions(&f.main);
        assert_eq!(rows[1].0, "nas.approval.expired");
        assert_eq!(rows[1].2, "expired");
        // A second sweep has nothing left to close.
        assert!(expire_due(&f.main, &f.nas, "node-test").is_empty());
    }

    /// Every refusal the Tasks queue can show is a code the screen words,
    /// never an English sentence (M1).
    #[test]
    fn every_approval_refusal_reaches_the_screen_as_a_code() {
        let cases = [
            (ApprovalError::NotFound, "refusal:approval_not_found"),
            (ApprovalError::OwnRequest, "refusal:approval_own_request"),
            (ApprovalError::Closed("approved".to_string()), "refusal:approval_already_approved"),
            (ApprovalError::Closed("rejected".to_string()), "refusal:approval_already_rejected"),
            (ApprovalError::Closed("expired".to_string()), "refusal:approval_already_expired"),
            (ApprovalError::Closed("closed".to_string()), "refusal:approval_closed"),
            (ApprovalError::Expired, "refusal:approval_expired"),
        ];
        for (error, code) in cases {
            assert_eq!(error.to_string(), code);
        }
    }

    #[test]
    fn a_pending_operation_raises_an_alert_that_the_decision_resolves() {
        let f = fixture(&["u-anna", "u-piotr"], &[]);
        let parked = park_destroy(&f, "u-anna");
        let open = store::list_alerts(&f.nas, true).expect("alerts");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].subject_kind, "approval");
        assert_eq!(open[0].subject_id, parked.request_id);
        // Coded: the operation and the subject a screen words, never the id.
        assert_eq!(open[0].code, "approval_pending");
        assert_eq!(open[0].params.get("operation").map(String::as_str), Some(parked.operation.as_str()));
        assert_eq!(open[0].params.get("subject").map(String::as_str), Some(parked.subject.as_str()));

        claim(&f.actor("u-piotr"), &parked.request_id).expect("claim");
        assert!(store::list_alerts(&f.nas, true).expect("alerts").is_empty());
    }

    /// Wave-4 round-2 critic M-B: a config import from an export without a
    /// node name is stored under the node's id, and the alert's English
    /// title — forwarded as written, and the screens' tooltip — read "a
    /// red-path operation on '<64 hex>' …". The title names what the caller
    /// resolved, or no subject; the id stays only in the stored parameter,
    /// which the read boundary resolves.
    #[test]
    fn a_parked_import_s_alert_title_names_the_node_or_nothing_never_its_id() {
        let f = fixture(&["u-anna", "u-piotr"], &[]);
        let node_id = "9f".repeat(32);
        let import = TentaNasPayload::ConfigImportApplyRequest { json: "{}".into(), sudo_password: None };
        let title_of = |shown: &str| {
            let parked = park_shown(&f.actor("u-anna"), OP_CONFIG_IMPORT, &node_id, shown, "overwrites 1: nightly", &import)
                .expect("park");
            let alert = store::list_alerts(&f.nas, true)
                .expect("alerts")
                .into_iter()
                .find(|a| a.subject_id == parked.request_id)
                .expect("the alert");
            assert_eq!(alert.params.get("subject"), Some(&node_id), "the stored subject is resolved on read");
            alert.title
        };
        assert_eq!(title_of(""), "a red-path operation waits for a second admin");
        assert_eq!(title_of("helios"), "a red-path operation on 'helios' waits for a second admin");
    }

    /// §5.10: a protection comes off through exactly TWO doors — the approval
    /// executor, and the single-admin red path the owner ruled on 2026-09-03,
    /// which only runs when `second_pair_available` says no second admin
    /// exists. The mistake this guards against is a THIRD call site, which no
    /// runtime test can see — so the invariant is checked against the crate's
    /// own source.
    #[test]
    fn nothing_outside_the_two_release_doors_can_lift_a_protection() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs(&src, &mut files);
        assert!(files.len() > 100, "the source walk found nothing: {}", files.len());

        let mentions = |needle: &str| -> Vec<String> {
            let mut hits: Vec<String> = files
                .iter()
                .filter(|(_, text)| text.contains(needle))
                .map(|(path, _)| path.clone())
                .collect();
            hits.sort();
            hits
        };
        // The catalog command is built by exactly one function, in the module
        // that owns snapshots.
        assert_eq!(mentions("release_command("), vec!["tentanas/snapshots.rs"]);
        // …and the job that runs it is started from exactly one other place.
        let callers = mentions("snapshots::spawn_release");
        assert_eq!(callers, vec!["dispatch/tentanas.rs"]);

        let dispatch = &files
            .iter()
            .find(|(path, _)| path == "dispatch/tentanas.rs")
            .expect("dispatch/tentanas.rs")
            .1;
        assert_eq!(
            dispatch.matches("spawn_release(").count(),
            2,
            "exactly two release call sites in the dispatcher"
        );
        let executor = dispatch
            .split_once("async fn execute_approved")
            .expect("the approval executor")
            .1;
        let executor = executor.split_once("\nfn variant_of").expect("its end").0;
        assert!(
            executor.contains("spawn_release("),
            "the release is started inside execute_approved, which only an approved claim reaches"
        );

        // The second door: the handler that answers the request directly, and
        // it reaches the release only past the admin count and the retype.
        let direct = dispatch
            .split_once("async fn snapshot_protection_release")
            .expect("the release handler")
            .1;
        let direct = direct.split_once("\nfn approvals_response").expect("its end").0;
        assert!(
            direct.contains("second_pair_available(") && direct.contains("require_confirm("),
            "the direct release is gated by the admin count and the retype"
        );
        let gate_at = direct
            .find("second_pair_available(")
            .expect("the admin count");
        let confirm_at = direct.find("require_confirm(").expect("the retype");
        let release_at = direct.find("spawn_release(").expect("the release");
        assert!(
            gate_at < confirm_at && confirm_at < release_at,
            "the count and the retype come BEFORE the release, not after it"
        );
    }

    /// Every `.rs` file under `src`, as (path relative to `src`, contents) —
    /// except this one, whose test above quotes the very names it looks for.
    fn collect_rs(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(dir).expect("src dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(&root)
                    .expect("under src")
                    .to_string_lossy()
                    .replace('\\', "/");
                if rel == "tentanas/approvals.rs" {
                    continue;
                }
                out.push((rel, std::fs::read_to_string(&path).expect("read")));
            }
        }
    }

    /// The owner's ruling of 2026-09-03: a fleet with one admin has no second
    /// pair of eyes, so the snapshot release runs directly there and still
    /// parks as soon as a second admin exists. The count is the LIVE
    /// membership, not a saved number and not the four-eyes switch.
    #[test]
    fn a_single_admin_fleet_releases_directly_and_two_admins_still_park() {
        // One admin plus a plain member holding the same grant: one pair.
        let one = fixture(&["u-anna"], &["u-jan"]);
        assert!(!second_pair_available(&one.actor("u-anna")));
        assert!(!second_pair_available(&one.actor("u-jan")));

        let two = fixture(&["u-anna", "u-piotr"], &[]);
        assert!(second_pair_available(&two.actor("u-anna")));

        // Switching four eyes OFF does not turn the release into a direct
        // path: the second pair still exists, so the hold still needs it.
        set_settings(&two.actor("u-anna"), false, 0).expect("save");
        assert!(!required(&two.actor("u-anna")));
        assert!(second_pair_available(&two.actor("u-anna")));

        // …and switching it ON on the single-admin fleet does not conjure a
        // second admin: the release would park with nobody able to release it.
        set_settings(&one.actor("u-anna"), true, 0).expect("save");
        assert!(required(&one.actor("u-anna")));
        assert!(!second_pair_available(&one.actor("u-anna")));

        // A second admin appearing flips it, with no setting touched.
        let admin_role = role_id(&one.main, "org_admin");
        crate::services::org::repo::add_membership(
            &one.main,
            &one.org_id,
            "u-piotr",
            &admin_role,
            "test",
        )
        .expect("membership");
        crate::db::repository::upsert_permission(
            &one.main,
            &one.addon_id,
            "user",
            "u-piotr",
            PERM_ADMIN,
            "allow",
            None,
        )
        .expect("grant");
        one.checker.refresh_all();
        assert!(second_pair_available(&one.actor("u-anna")));
    }

    /// TentaNas is one instance and one `tentanas.db` per node, shared by
    /// every organisation on it. Org B's admins are real approvers — in B —
    /// and must neither see org A's parked request nor be able to decide it:
    /// every answer B gets is the one a request id that does not exist gets,
    /// whether A's request is pending, decided or expired.
    #[test]
    fn another_organisations_request_is_invisible_and_answers_like_a_missing_one() {
        let f = fixture(&["u-anna", "u-piotr"], &[]);
        let beta = f.second_org(&["u-ewa", "u-marek"]);
        assert_eq!(
            approver_ids(&f.main, &f.checker, &beta, &f.addon_id).len(),
            2,
            "B's admins are approvers of their own organisation"
        );
        let parked = park_destroy(&f, "u-anna");
        let missing = |user: &str| {
            let b = f.actor_in(&beta, user);
            (
                claim(&b, &parked.request_id).unwrap_err(),
                reject(&b, &parked.request_id, "nie").unwrap_err(),
                claim(&b, "nie-ma").unwrap_err(),
            )
        };
        let not_found = (ApprovalError::NotFound, ApprovalError::NotFound, ApprovalError::NotFound);

        // Pending: B sees nothing and cannot approve or reject it.
        assert!(list(&f.actor_in(&beta, "u-ewa"), true).expect("list").is_empty());
        assert_eq!(missing("u-ewa"), not_found);
        // …and B's attempts changed nothing: A's second admin still decides.
        let open = list(&f.actor("u-piotr"), false).expect("list");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].status, "pending");

        // B's own request is B's: listed for B, never for A.
        let own = park(
            &f.actor_in(&beta, "u-ewa"),
            OP_ELASTIC_DESTROY,
            "bravo",
            "rozwiązuje macierz bravo",
            &TentaNasPayload::ElasticArrayDestroyRequest {
                name: "bravo".into(),
                confirm_name: "bravo".into(),
                sudo_password: None,
            },
        )
        .expect("park");
        let seen_by_b: Vec<String> = list(&f.actor_in(&beta, "u-marek"), true)
            .expect("list")
            .into_iter()
            .map(|r| r.request_id)
            .collect();
        assert_eq!(seen_by_b, vec![own.request_id.clone()]);
        assert!(list(&f.actor("u-piotr"), true)
            .expect("list")
            .iter()
            .all(|r| r.request_id != own.request_id));
        assert_eq!(
            claim(&f.actor("u-piotr"), &own.request_id).unwrap_err(),
            ApprovalError::NotFound
        );

        // Decided: still "not found" for B, never "already rejected".
        reject(&f.actor("u-piotr"), &parked.request_id, "nie teraz").expect("reject");
        assert_eq!(missing("u-marek"), not_found);

        // Expired: still "not found" for B, never "expired".
        let late = park_destroy(&f, "u-anna");
        {
            let conn = f.nas.write().expect("write");
            conn.execute(
                "UPDATE nas_pending_approvals SET expires_at = ?2 WHERE request_id = ?1",
                rusqlite::params![late.request_id, "2020-01-01T00:00:00Z"],
            )
            .expect("expire");
        }
        assert_eq!(
            claim(&f.actor_in(&beta, "u-ewa"), &late.request_id).unwrap_err(),
            ApprovalError::NotFound
        );
        // The request itself is A's and still closes for A as expired.
        assert_eq!(
            claim(&f.actor("u-piotr"), &late.request_id).unwrap_err(),
            ApprovalError::Expired
        );
    }

    /// The four-eyes switch is per organisation: org B turning it OFF for
    /// itself leaves org A's red paths parking, and A's saved TTL is A's.
    #[test]
    fn four_eyes_is_switched_per_organisation() {
        let f = fixture(&["u-anna", "u-piotr"], &[]);
        let beta = f.second_org(&["u-ewa", "u-marek"]);
        set_settings(&f.actor("u-anna"), true, 6).expect("A saves");
        let b = set_settings(&f.actor_in(&beta, "u-ewa"), false, 48).expect("B saves");
        assert!(!b.enabled && b.ttl_hours == 48);
        assert!(!required(&f.actor_in(&beta, "u-marek")));

        let a = f.settings();
        assert!(a.enabled && !a.by_default, "B's switch reached A: {a:?}");
        assert_eq!(a.ttl_hours, 6);
        assert!(required(&f.actor("u-piotr")));
        // …and a parked request of A carries A's TTL, not B's.
        let parked = park_destroy(&f, "u-anna");
        let requested = chrono::DateTime::parse_from_rfc3339(&parked.requested_at).expect("at");
        let expires = chrono::DateTime::parse_from_rfc3339(&parked.expires_at).expect("at");
        assert_eq!((expires - requested).num_hours(), 6);
    }

    #[test]
    fn deciding_a_request_that_is_not_here_is_not_found() {
        let f = fixture(&["u-anna", "u-piotr"], &[]);
        assert_eq!(
            claim(&f.actor("u-piotr"), "nie-ma").unwrap_err(),
            ApprovalError::NotFound
        );
    }
}
