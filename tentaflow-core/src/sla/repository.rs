//! Org-scoped storage of SLA policies and assignments, with the Sync Ledger
//! side of both tables: the capture a local write records, the apply of an
//! operation that arrived from a peer, and the baseline reseed.
//!
//! Every function takes the organization and treats a row of another one as
//! missing, so the caller answers `not_found` without revealing that the id
//! exists elsewhere. A change that commits (locally or from the mesh) bumps
//! the SLA generation, which is what makes the registry rebuild its gates.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, Transaction};

use super::policy::{
    BusOrdering, SlaAssignment, SlaPolicy, SlaPolicySpec, MAX_DESCRIPTION_CHARS, MAX_NAME_CHARS,
    SLA_TARGET_FLOW,
};
use crate::db::repository::record_core_capture_for_org_tx;
use crate::db::DbPool;
use crate::sync::core_materializer::sql_error;
use crate::sync::core_materializer::{field_string, optional_present_i64, optional_present_string};
use crate::sync::core_registry::CoreSyncResourceKind as Kind;
use crate::sync::ledger::{ActionType, FieldValue, LedgerResult, SyncLedgerError, SyncOperation};
use crate::sync::resource_id::{composite_resource_id, decode_segments};
use crate::sync::runtime::SqlWriteAction;

pub type Result<T> = std::result::Result<T, SlaError>;

#[derive(Debug, thiserror::Error)]
pub enum SlaError {
    #[error("{entity} not found: {id}")]
    NotFound { entity: &'static str, id: String },
    /// The policy still governs at least one service.
    #[error("policy {policy_id} is assigned to {assignments} service(s)")]
    PolicyInUse { policy_id: String, assignments: u32 },
    #[error("a policy named '{0}' already exists in this organization")]
    NameTaken(String),
    /// Flows are node-global, so another organization may already govern the
    /// flow; that assignment is not ours to replace or lift.
    #[error("service {0} is governed by another organization")]
    GovernedElsewhere(String),
    /// A platform flow (`flows.is_system`): not the administrator's to govern.
    #[error("system flow {0} cannot be governed by a policy")]
    SystemFlow(String),
    #[error("invalid value for {field}: {reason}")]
    Invalid { field: &'static str, reason: String },
    #[error("storage error: {0}")]
    Storage(String),
}

impl SlaError {
    /// Stable code for the wire and for the UI to translate.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not_found",
            Self::PolicyInUse { .. } => "policy_in_use",
            Self::NameTaken(_) => "name_taken",
            Self::SystemFlow(_) => "system_flow",
            Self::GovernedElsewhere(_) => "governed_elsewhere",
            Self::Invalid { .. } => "invalid_value",
            Self::Storage(_) => "storage_error",
        }
    }
}

impl From<rusqlite::Error> for SlaError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<crate::db::DbError> for SlaError {
    fn from(error: crate::db::DbError) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<anyhow::Error> for SlaError {
    fn from(error: anyhow::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

const SPEC_COLUMNS: &str = "name, description, max_concurrency, max_queue, queue_timeout_ms, \
     call_limit, call_window_secs, timeout_ms, latency_p95_ms, error_rate_pct, warn_at_pct, ordering";
const POLICY_SELECT: &str = "SELECT id, org_id, name, description, max_concurrency, max_queue, \
     queue_timeout_ms, call_limit, call_window_secs, timeout_ms, latency_p95_ms, error_rate_pct, \
     warn_at_pct, ordering, created_at_ms, updated_at_ms, updated_by FROM service_sla_policies";
const ASSIGNMENT_SELECT: &str =
    "SELECT target_kind, target_id, policy_id, org_id, assigned_at_ms, \
     assigned_by FROM service_sla_assignments";

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn is_unique_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    )
}

fn spec_from_row(r: &rusqlite::Row<'_>, base: usize) -> rusqlite::Result<SlaPolicySpec> {
    let ordering: String = r.get(base + 11)?;
    Ok(SlaPolicySpec {
        name: r.get(base)?,
        description: r.get(base + 1)?,
        max_concurrency: r.get(base + 2)?,
        max_queue: r.get(base + 3)?,
        queue_timeout_ms: r.get(base + 4)?,
        call_limit: r.get(base + 5)?,
        call_window_secs: r.get(base + 6)?,
        timeout_ms: r.get(base + 7)?,
        latency_p95_ms: r.get(base + 8)?,
        error_rate_pct: r.get(base + 9)?,
        warn_at_pct: r.get(base + 10)?,
        ordering: BusOrdering::parse(&ordering).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                base + 11,
                rusqlite::types::Type::Text,
                format!("unknown ordering '{ordering}'").into(),
            )
        })?,
    })
}

fn row_to_policy(r: &rusqlite::Row<'_>) -> rusqlite::Result<SlaPolicy> {
    Ok(SlaPolicy {
        id: r.get(0)?,
        org_id: r.get(1)?,
        spec: spec_from_row(r, 2)?,
        created_at_ms: r.get(14)?,
        updated_at_ms: r.get(15)?,
        updated_by: r.get(16)?,
    })
}

fn row_to_assignment(r: &rusqlite::Row<'_>) -> rusqlite::Result<SlaAssignment> {
    Ok(SlaAssignment {
        target_kind: r.get(0)?,
        target_id: r.get(1)?,
        policy_id: r.get(2)?,
        org_id: r.get(3)?,
        assigned_at_ms: r.get(4)?,
        assigned_by: r.get(5)?,
    })
}

fn policy_in_tx(tx: &Transaction<'_>, org_id: &str, id: &str) -> Result<Option<SlaPolicy>> {
    Ok(tx
        .query_row(
            &format!("{POLICY_SELECT} WHERE id = ?1 AND org_id = ?2"),
            rusqlite::params![id, org_id],
            row_to_policy,
        )
        .optional()?)
}

fn assignment_in_tx(
    tx: &Transaction<'_>,
    org_id: &str,
    target_kind: &str,
    target_id: &str,
) -> Result<Option<SlaAssignment>> {
    Ok(tx
        .query_row(
            &format!(
                "{ASSIGNMENT_SELECT} WHERE target_kind = ?1 AND target_id = ?2 AND org_id = ?3"
            ),
            rusqlite::params![target_kind, target_id, org_id],
            row_to_assignment,
        )
        .optional()?)
}

pub fn list_policies(pool: &DbPool, org_id: &str) -> Result<Vec<SlaPolicy>> {
    let conn = pool.read()?;
    let mut stmt = conn.prepare(&format!("{POLICY_SELECT} WHERE org_id = ?1 ORDER BY name"))?;
    let rows = stmt
        .query_map([org_id], row_to_policy)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_policy(pool: &DbPool, org_id: &str, id: &str) -> Result<Option<SlaPolicy>> {
    let conn = pool.read()?;
    Ok(conn
        .query_row(
            &format!("{POLICY_SELECT} WHERE id = ?1 AND org_id = ?2"),
            rusqlite::params![id, org_id],
            row_to_policy,
        )
        .optional()?)
}

pub fn list_assignments(pool: &DbPool, org_id: &str) -> Result<Vec<SlaAssignment>> {
    let conn = pool.read()?;
    let mut stmt = conn.prepare(&format!(
        "{ASSIGNMENT_SELECT} WHERE org_id = ?1 ORDER BY target_kind, target_id"
    ))?;
    let rows = stmt
        .query_map([org_id], row_to_assignment)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The assignments other organizations hold. Flows are node-global, so what
/// governs a flow the caller's organization also calls is the caller's to see
/// as a state, never as a policy: the caller reads the state only.
pub fn list_assignments_outside(pool: &DbPool, org_id: &str) -> Result<Vec<SlaAssignment>> {
    let conn = pool.read()?;
    let mut stmt = conn.prepare(&format!(
        "{ASSIGNMENT_SELECT} WHERE org_id <> ?1 ORDER BY target_kind, target_id"
    ))?;
    let rows = stmt
        .query_map([org_id], row_to_assignment)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_assignment(
    pool: &DbPool,
    org_id: &str,
    target_kind: &str,
    target_id: &str,
) -> Result<Option<SlaAssignment>> {
    let conn = pool.read()?;
    Ok(conn
        .query_row(
            &format!(
                "{ASSIGNMENT_SELECT} WHERE target_kind = ?1 AND target_id = ?2 AND org_id = ?3"
            ),
            rusqlite::params![target_kind, target_id, org_id],
            row_to_assignment,
        )
        .optional()?)
}

/// One assignment with the policy that governs it, as the registry needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveAssignment {
    pub target_kind: String,
    pub target_id: String,
    pub policy_id: String,
    pub org_id: String,
    pub spec: SlaPolicySpec,
}

/// Every assignment of every organization: the gates of a node are not scoped
/// to one organization, a flow is governed whoever calls it.
pub fn load_active(pool: &DbPool) -> Result<Vec<ActiveAssignment>> {
    let conn = pool.read()?;
    let mut stmt = conn.prepare(
        "SELECT a.target_kind, a.target_id, a.policy_id, a.org_id, \
                p.name, p.description, p.max_concurrency, p.max_queue, p.queue_timeout_ms, \
                p.call_limit, p.call_window_secs, p.timeout_ms, p.latency_p95_ms, \
                p.error_rate_pct, p.warn_at_pct, p.ordering \
           FROM service_sla_assignments a \
           JOIN service_sla_policies p ON p.id = a.policy_id \
          ORDER BY a.target_kind, a.target_id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(ActiveAssignment {
                target_kind: r.get(0)?,
                target_id: r.get(1)?,
                policy_id: r.get(2)?,
                org_id: r.get(3)?,
                spec: spec_from_row(r, 4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn opt_field(value: Option<u32>) -> FieldValue {
    value.map_or(FieldValue::Null, |v| FieldValue::I64(i64::from(v)))
}

/// Ledger fields of a policy row. Mirrors `apply_policy`: peers materialize
/// exactly these names, so the two lists move together.
fn policy_fields(policy: &SlaPolicy) -> BTreeMap<String, FieldValue> {
    let spec = &policy.spec;
    let mut f = BTreeMap::new();
    f.insert("org_id".into(), FieldValue::String(policy.org_id.clone()));
    f.insert("name".into(), FieldValue::String(spec.name.clone()));
    f.insert(
        "description".into(),
        FieldValue::String(spec.description.clone()),
    );
    f.insert("max_concurrency".into(), opt_field(spec.max_concurrency));
    f.insert("max_queue".into(), opt_field(spec.max_queue));
    f.insert("queue_timeout_ms".into(), opt_field(spec.queue_timeout_ms));
    f.insert("call_limit".into(), opt_field(spec.call_limit));
    f.insert("call_window_secs".into(), opt_field(spec.call_window_secs));
    f.insert("timeout_ms".into(), opt_field(spec.timeout_ms));
    f.insert("latency_p95_ms".into(), opt_field(spec.latency_p95_ms));
    f.insert("error_rate_pct".into(), opt_field(spec.error_rate_pct));
    f.insert("warn_at_pct".into(), opt_field(spec.warn_at_pct));
    f.insert(
        "ordering".into(),
        FieldValue::String(spec.ordering.as_str().into()),
    );
    f.insert(
        "created_at_ms".into(),
        FieldValue::I64(policy.created_at_ms),
    );
    f.insert(
        "updated_at_ms".into(),
        FieldValue::I64(policy.updated_at_ms),
    );
    f.insert(
        "updated_by".into(),
        FieldValue::String(policy.updated_by.clone()),
    );
    f
}

fn assignment_resource_id(target_kind: &str, target_id: &str) -> String {
    composite_resource_id(&[target_kind, target_id])
}

fn assignment_fields(a: &SlaAssignment) -> BTreeMap<String, FieldValue> {
    let mut f = BTreeMap::new();
    f.insert("org_id".into(), FieldValue::String(a.org_id.clone()));
    f.insert(
        "target_kind".into(),
        FieldValue::String(a.target_kind.clone()),
    );
    f.insert("target_id".into(), FieldValue::String(a.target_id.clone()));
    f.insert("policy_id".into(), FieldValue::String(a.policy_id.clone()));
    f.insert("assigned_at_ms".into(), FieldValue::I64(a.assigned_at_ms));
    f.insert(
        "assigned_by".into(),
        FieldValue::String(a.assigned_by.clone()),
    );
    f
}

pub fn create_policy(
    pool: &DbPool,
    org_id: &str,
    actor: &str,
    spec: &SlaPolicySpec,
) -> Result<SlaPolicy> {
    spec.validate()?;
    let spec = SlaPolicySpec {
        name: spec.name.trim().to_string(),
        ..spec.clone()
    };
    let id = uuid::Uuid::now_v7().to_string();
    let now = now_ms();
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    tx.execute(
        &format!(
            "INSERT INTO service_sla_policies (id, org_id, {SPEC_COLUMNS}, created_at_ms, \
             updated_at_ms, updated_by) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15, ?16)"
        ),
        rusqlite::params![
            id,
            org_id,
            spec.name,
            spec.description,
            spec.max_concurrency,
            spec.max_queue,
            spec.queue_timeout_ms,
            spec.call_limit,
            spec.call_window_secs,
            spec.timeout_ms,
            spec.latency_p95_ms,
            spec.error_rate_pct,
            spec.warn_at_pct,
            spec.ordering.as_str(),
            now,
            actor,
        ],
    )
    .map_err(|e| {
        if is_unique_violation(&e) {
            SlaError::NameTaken(spec.name.clone())
        } else {
            e.into()
        }
    })?;
    let stored = policy_in_tx(&tx, org_id, &id)?.ok_or_else(|| SlaError::NotFound {
        entity: "policy",
        id: id.clone(),
    })?;
    record_core_capture_for_org_tx(
        &tx,
        Kind::SlaPolicy,
        org_id,
        id,
        SqlWriteAction::Insert,
        policy_fields(&stored),
        Some(actor.to_string()),
    )?;
    tx.commit()?;
    super::bump_generation();
    Ok(stored)
}

pub fn update_policy(
    pool: &DbPool,
    org_id: &str,
    actor: &str,
    id: &str,
    spec: &SlaPolicySpec,
) -> Result<SlaPolicy> {
    spec.validate()?;
    let spec = SlaPolicySpec {
        name: spec.name.trim().to_string(),
        ..spec.clone()
    };
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let changed = tx
        .execute(
            "UPDATE service_sla_policies SET name = ?3, description = ?4, max_concurrency = ?5, \
                max_queue = ?6, queue_timeout_ms = ?7, call_limit = ?8, call_window_secs = ?9, \
                timeout_ms = ?10, latency_p95_ms = ?11, error_rate_pct = ?12, warn_at_pct = ?13, \
                ordering = ?14, updated_at_ms = ?15, updated_by = ?16 \
              WHERE id = ?1 AND org_id = ?2",
            rusqlite::params![
                id,
                org_id,
                spec.name,
                spec.description,
                spec.max_concurrency,
                spec.max_queue,
                spec.queue_timeout_ms,
                spec.call_limit,
                spec.call_window_secs,
                spec.timeout_ms,
                spec.latency_p95_ms,
                spec.error_rate_pct,
                spec.warn_at_pct,
                spec.ordering.as_str(),
                now_ms(),
                actor,
            ],
        )
        .map_err(|e| {
            if is_unique_violation(&e) {
                SlaError::NameTaken(spec.name.clone())
            } else {
                e.into()
            }
        })?;
    if changed == 0 {
        return Err(SlaError::NotFound {
            entity: "policy",
            id: id.to_string(),
        });
    }
    let stored = policy_in_tx(&tx, org_id, id)?.ok_or_else(|| SlaError::NotFound {
        entity: "policy",
        id: id.to_string(),
    })?;
    record_core_capture_for_org_tx(
        &tx,
        Kind::SlaPolicy,
        org_id,
        id.to_string(),
        SqlWriteAction::Update,
        policy_fields(&stored),
        Some(actor.to_string()),
    )?;
    tx.commit()?;
    super::bump_generation();
    Ok(stored)
}

/// Refused with `policy_in_use` while any service is assigned to the policy:
/// deleting it would silently lift the limits of those services.
pub fn delete_policy(pool: &DbPool, org_id: &str, actor: &str, id: &str) -> Result<()> {
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    if policy_in_tx(&tx, org_id, id)?.is_none() {
        return Err(SlaError::NotFound {
            entity: "policy",
            id: id.to_string(),
        });
    }
    let assignments: u32 = tx.query_row(
        "SELECT COUNT(*) FROM service_sla_assignments WHERE policy_id = ?1",
        [id],
        |r| r.get(0),
    )?;
    if assignments > 0 {
        return Err(SlaError::PolicyInUse {
            policy_id: id.to_string(),
            assignments,
        });
    }
    tx.execute(
        "DELETE FROM service_sla_policies WHERE id = ?1 AND org_id = ?2",
        rusqlite::params![id, org_id],
    )?;
    record_core_capture_for_org_tx(
        &tx,
        Kind::SlaPolicy,
        org_id,
        id.to_string(),
        SqlWriteAction::Delete,
        BTreeMap::new(),
        Some(actor.to_string()),
    )?;
    tx.commit()?;
    super::bump_generation();
    Ok(())
}

/// What an assignment changed: the row it replaced, read in the same
/// transaction as the write, and the row now in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assigned {
    pub previous: Option<SlaAssignment>,
    pub current: SlaAssignment,
}

/// Binds `policy_id` to the service, replacing any policy it had.
/// `GovernedElsewhere` when another organization already governs it.
pub fn assign(
    pool: &DbPool,
    org_id: &str,
    actor: &str,
    target_kind: &str,
    target_id: &str,
    policy_id: &str,
) -> Result<Assigned> {
    if target_kind != SLA_TARGET_FLOW {
        return Err(SlaError::Invalid {
            field: "target_kind",
            reason: format!("only '{SLA_TARGET_FLOW}' can be assigned, got '{target_kind}'"),
        });
    }
    if target_id.trim().is_empty() {
        return Err(SlaError::Invalid {
            field: "target_id",
            reason: "must not be empty".into(),
        });
    }
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    // Flows carry no organization of their own: they are default-org scoped and
    // any role that may edit flows may edit any of them (`flow_update` checks
    // the role and `is_system`, nothing else). So the rule enforced here is the
    // one the codebase has: the flow must exist and must not be a platform one.
    // A missing flow and a flow of another organization's assignment answer with
    // the same code, so the response does not tell which it was.
    match tx
        .query_row(
            "SELECT is_system FROM flows WHERE id = ?1",
            [target_id],
            |r| r.get::<_, bool>(0),
        )
        .optional()?
    {
        None => {
            return Err(SlaError::NotFound {
                entity: "service",
                id: target_id.to_string(),
            })
        }
        Some(true) => return Err(SlaError::SystemFlow(target_id.to_string())),
        Some(false) => {}
    }
    if policy_in_tx(&tx, org_id, policy_id)?.is_none() {
        return Err(SlaError::NotFound {
            entity: "policy",
            id: policy_id.to_string(),
        });
    }
    // The service may already be governed by another organization's policy;
    // that row is not ours to replace.
    let owner: Option<String> = tx
        .query_row(
            "SELECT org_id FROM service_sla_assignments WHERE target_kind = ?1 AND target_id = ?2",
            rusqlite::params![target_kind, target_id],
            |r| r.get(0),
        )
        .optional()?;
    if owner.as_deref().is_some_and(|owner| owner != org_id) {
        return Err(SlaError::GovernedElsewhere(target_id.to_string()));
    }
    let previous = assignment_in_tx(&tx, org_id, target_kind, target_id)?;
    tx.execute(
        "INSERT INTO service_sla_assignments \
           (target_kind, target_id, policy_id, org_id, assigned_at_ms, assigned_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(target_kind, target_id) DO UPDATE SET \
            policy_id = excluded.policy_id, assigned_at_ms = excluded.assigned_at_ms, \
            assigned_by = excluded.assigned_by",
        rusqlite::params![target_kind, target_id, policy_id, org_id, now_ms(), actor],
    )?;
    let stored = assignment_in_tx(&tx, org_id, target_kind, target_id)?.ok_or_else(|| {
        SlaError::NotFound {
            entity: "assignment",
            id: target_id.to_string(),
        }
    })?;
    record_core_capture_for_org_tx(
        &tx,
        Kind::SlaAssignment,
        org_id,
        assignment_resource_id(target_kind, target_id),
        if previous.is_some() {
            SqlWriteAction::Update
        } else {
            SqlWriteAction::Insert
        },
        assignment_fields(&stored),
        Some(actor.to_string()),
    )?;
    tx.commit()?;
    super::bump_generation();
    Ok(Assigned {
        previous,
        current: stored,
    })
}

/// Lifts the policy from a service and returns the assignment it removed, read
/// in the same transaction; `None` when it had none. `GovernedElsewhere` when
/// another organization's policy governs it.
pub fn unassign(
    pool: &DbPool,
    org_id: &str,
    actor: &str,
    target_kind: &str,
    target_id: &str,
) -> Result<Option<SlaAssignment>> {
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let Some(previous) = assignment_in_tx(&tx, org_id, target_kind, target_id)? else {
        let foreign: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM service_sla_assignments \
              WHERE target_kind = ?1 AND target_id = ?2)",
            rusqlite::params![target_kind, target_id],
            |r| r.get(0),
        )?;
        return if foreign {
            Err(SlaError::GovernedElsewhere(target_id.to_string()))
        } else {
            Ok(None)
        };
    };
    tx.execute(
        "DELETE FROM service_sla_assignments \
          WHERE target_kind = ?1 AND target_id = ?2 AND org_id = ?3",
        rusqlite::params![target_kind, target_id, org_id],
    )?;
    record_core_capture_for_org_tx(
        &tx,
        Kind::SlaAssignment,
        org_id,
        assignment_resource_id(target_kind, target_id),
        SqlWriteAction::Delete,
        BTreeMap::new(),
        Some(actor.to_string()),
    )?;
    tx.commit()?;
    super::bump_generation();
    Ok(Some(previous))
}

// -----------------------------------------------------------------------------
// Sync Ledger: apply and reseed
// -----------------------------------------------------------------------------

fn runtime(message: impl Into<String>) -> SyncLedgerError {
    SyncLedgerError::Runtime(message.into())
}

fn optional_u32(operation: &SyncOperation, key: &str) -> LedgerResult<Option<u32>> {
    optional_present_i64(operation, key)?
        .map(|v| u32::try_from(v).map_err(|_| runtime(format!("field {key} out of range: {v}"))))
        .transpose()
}

/// The organization a row says it belongs to must be the one the envelope is
/// signed for: a peer cannot write into another organization through its own.
fn require_envelope_org(operation: &SyncOperation) -> LedgerResult<String> {
    let org_id = field_string(operation, "org_id")?;
    if org_id != operation.body.org_id {
        return Err(runtime(format!(
            "sla row names organization {org_id}, its envelope names {}",
            operation.body.org_id
        )));
    }
    Ok(org_id)
}

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// `base` made unique by the random tail of the policy's UUIDv7 (its leading
/// hex digits are a timestamp, so policies created in the same minute would
/// share them), truncated so the result still fits the name bound.
fn distinguished_name(base: &str, id: &str) -> String {
    let hex: Vec<char> = id.chars().filter(|c| *c != '-').collect();
    let tail: String = hex[hex.len().saturating_sub(8)..].iter().collect();
    let suffix = format!(" ({tail})");
    let keep = MAX_NAME_CHARS.saturating_sub(suffix.chars().count());
    format!("{}{suffix}", truncate_chars(base, keep))
}

/// Dispatches one replicated operation of the two SLA tables. Returns the rows
/// that changed; a write the organization check refused changes none.
pub(crate) fn apply(
    tx: &Transaction<'_>,
    kind: Kind,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    match kind {
        Kind::SlaPolicy => apply_policy(tx, operation),
        Kind::SlaAssignment => apply_assignment(tx, operation),
        other => Err(runtime(format!("{other:?} is not an SLA resource"))),
    }
}

fn apply_policy(tx: &Transaction<'_>, operation: &SyncOperation) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
    let envelope_org = &operation.body.org_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => {
            let org_id = require_envelope_org(operation)?;
            let ordering = optional_present_string(operation, "ordering")?
                .unwrap_or_else(|| BusOrdering::Partition.as_str().to_string());
            let ordering = BusOrdering::parse(&ordering)
                .ok_or_else(|| runtime(format!("unknown ordering '{ordering}'")))?;
            let created_at_ms = optional_present_i64(operation, "created_at_ms")?.unwrap_or(0);
            let mut name = truncate_chars(&field_string(operation, "name")?, MAX_NAME_CHARS);
            // Two nodes can create policies of the same name before they see
            // each other. The name is unique per organization, and an operation
            // that cannot be applied would block the stream behind it, so the
            // clash is settled the same way on every node: the older policy
            // (`created_at_ms`, then id) keeps the plain name, the other one is
            // told apart by its own id. When the incoming row is the older one,
            // the local row is renamed here, without a capture: the other node
            // makes the same decision from the same two rows.
            let clash: Option<(String, i64)> = tx
                .query_row(
                    "SELECT id, created_at_ms FROM service_sla_policies \
                      WHERE org_id = ?1 AND name = ?2 AND id <> ?3",
                    rusqlite::params![org_id, name, id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(sql_error)?;
            if let Some((other_id, other_created_ms)) = clash {
                if (created_at_ms, id.as_str()) < (other_created_ms, other_id.as_str()) {
                    tx.execute(
                        "UPDATE service_sla_policies SET name = ?2 WHERE id = ?1",
                        rusqlite::params![other_id, distinguished_name(&name, &other_id)],
                    )
                    .map_err(sql_error)?;
                } else {
                    name = distinguished_name(&name, id);
                }
            }
            tx.execute(
                &format!(
                    "INSERT INTO service_sla_policies (id, org_id, {SPEC_COLUMNS}, created_at_ms, \
                     updated_at_ms, updated_by) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17) \
                     ON CONFLICT(id) DO UPDATE SET \
                        name = excluded.name, description = excluded.description, \
                        max_concurrency = excluded.max_concurrency, max_queue = excluded.max_queue, \
                        queue_timeout_ms = excluded.queue_timeout_ms, call_limit = excluded.call_limit, \
                        call_window_secs = excluded.call_window_secs, timeout_ms = excluded.timeout_ms, \
                        latency_p95_ms = excluded.latency_p95_ms, \
                        error_rate_pct = excluded.error_rate_pct, warn_at_pct = excluded.warn_at_pct, \
                        ordering = excluded.ordering, updated_at_ms = excluded.updated_at_ms, \
                        updated_by = excluded.updated_by \
                      WHERE service_sla_policies.org_id = excluded.org_id"
                ),
                rusqlite::params![
                    id,
                    org_id,
                    name,
                    truncate_chars(
                        &optional_present_string(operation, "description")?.unwrap_or_default(),
                        MAX_DESCRIPTION_CHARS,
                    ),
                    optional_u32(operation, "max_concurrency")?,
                    optional_u32(operation, "max_queue")?,
                    optional_u32(operation, "queue_timeout_ms")?,
                    optional_u32(operation, "call_limit")?,
                    optional_u32(operation, "call_window_secs")?,
                    optional_u32(operation, "timeout_ms")?,
                    optional_u32(operation, "latency_p95_ms")?,
                    optional_u32(operation, "error_rate_pct")?,
                    optional_u32(operation, "warn_at_pct")?,
                    ordering.as_str(),
                    created_at_ms,
                    optional_present_i64(operation, "updated_at_ms")?.unwrap_or(0),
                    optional_present_string(operation, "updated_by")?.unwrap_or_default(),
                ],
            )
            .map_err(sql_error)
        }
        ActionType::Delete => {
            // The origin refuses to delete a policy that is assigned, so an
            // assignment still here belongs to a delete that has not arrived
            // yet; leaving it would turn a foreign key into a stuck stream.
            let orphaned = tx
                .execute(
                    "DELETE FROM service_sla_assignments WHERE policy_id = ?1 AND org_id = ?2",
                    rusqlite::params![id, envelope_org],
                )
                .map_err(sql_error)?;
            if orphaned > 0 {
                tracing::warn!(
                    policy_id = %id,
                    removed_assignments = orphaned,
                    "a replicated policy delete removed assignments that were still in place"
                );
            }
            tx.execute(
                "DELETE FROM service_sla_policies WHERE id = ?1 AND org_id = ?2",
                rusqlite::params![id, envelope_org],
            )
            .map_err(sql_error)
        }
    }
}

/// `(target_kind, target_id)` of an assignment operation, from the resource id
/// the capture wrote; the fields, when present, must agree with it.
fn assignment_key(operation: &SyncOperation) -> LedgerResult<(String, String)> {
    let segments = decode_segments(&operation.body.resource_id)
        .filter(|s| s.len() == 2)
        .ok_or_else(|| runtime("malformed sla assignment resource id"))?;
    let (kind, target) = (segments[0].to_string(), segments[1].to_string());
    for (key, expected) in [("target_kind", &kind), ("target_id", &target)] {
        if let Some(value) = optional_present_string(operation, key)? {
            if &value != expected {
                return Err(runtime(format!(
                    "sla assignment {key} disagrees with its resource id"
                )));
            }
        }
    }
    Ok((kind, target))
}

fn apply_assignment(tx: &Transaction<'_>, operation: &SyncOperation) -> LedgerResult<usize> {
    let (target_kind, target_id) = assignment_key(operation)?;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => {
            let org_id = require_envelope_org(operation)?;
            let policy_id = field_string(operation, "policy_id")?;
            let policy_org: Option<String> = tx
                .query_row(
                    "SELECT org_id FROM service_sla_policies WHERE id = ?1",
                    [&policy_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_error)?;
            match policy_org {
                None => {
                    // A policy that has a version row but no row was deleted;
                    // this assignment lost a race with that delete, and waiting
                    // for the policy would block the stream for good.
                    let deleted: bool = tx
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM core_resource_versions \
                              WHERE resource_type = 'core.sla_policy' AND resource_id = ?1)",
                            [&policy_id],
                            |r| r.get(0),
                        )
                        .map_err(sql_error)?;
                    if deleted {
                        return Ok(0);
                    }
                    return Err(SyncLedgerError::DeferredOrdering(format!(
                        "sla policy not found: {policy_id}"
                    )));
                }
                Some(owner) if owner != org_id => {
                    return Err(runtime(
                        "sla assignment names a policy of another organization",
                    ))
                }
                Some(_) => {}
            }
            tx.execute(
                "INSERT INTO service_sla_assignments \
                   (target_kind, target_id, policy_id, org_id, assigned_at_ms, assigned_by) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(target_kind, target_id) DO UPDATE SET \
                    policy_id = excluded.policy_id, org_id = excluded.org_id, \
                    assigned_at_ms = excluded.assigned_at_ms, assigned_by = excluded.assigned_by",
                rusqlite::params![
                    target_kind,
                    target_id,
                    policy_id,
                    org_id,
                    optional_present_i64(operation, "assigned_at_ms")?.unwrap_or(0),
                    optional_present_string(operation, "assigned_by")?.unwrap_or_default(),
                ],
            )
            .map_err(sql_error)
        }
        ActionType::Delete => tx
            .execute(
                "DELETE FROM service_sla_assignments \
                  WHERE target_kind = ?1 AND target_id = ?2",
                rusqlite::params![target_kind, target_id],
            )
            .map_err(sql_error),
    }
}

/// Whether the policy the operation names exists and belongs to another
/// organization than the envelope's. Such a write changed nothing and must not
/// earn a place in the last-writer-wins order.
pub(crate) fn row_is_owned_elsewhere(
    tx: &Transaction<'_>,
    kind: Kind,
    operation: &SyncOperation,
) -> LedgerResult<bool> {
    let owner: Option<String> = match kind {
        // An assignment is ordered by last-writer-wins on its service alone,
        // whichever organization wrote it, so no write of it is "foreign".
        Kind::SlaAssignment => return Ok(false),
        Kind::SlaPolicy => tx
            .query_row(
                "SELECT org_id FROM service_sla_policies WHERE id = ?1",
                [&operation.body.resource_id],
                |r| r.get(0),
            )
            .optional(),
        other => return Err(runtime(format!("{other:?} is not an SLA resource"))),
    }
    .map_err(sql_error)?;
    Ok(owner.is_some_and(|owner| owner != operation.body.org_id))
}

/// Records one insert capture per stored row, policies before assignments in
/// descriptor order, for the baseline reseed. Returns how many were recorded.
pub(crate) fn reseed(tx: &Transaction<'_>, kind: Kind) -> anyhow::Result<usize> {
    match kind {
        Kind::SlaPolicy => {
            let policies = {
                let mut stmt = tx.prepare(&format!("{POLICY_SELECT} ORDER BY id"))?;
                let rows = stmt
                    .query_map([], row_to_policy)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            for policy in &policies {
                record_core_capture_for_org_tx(
                    tx,
                    Kind::SlaPolicy,
                    &policy.org_id,
                    policy.id.clone(),
                    SqlWriteAction::Insert,
                    policy_fields(policy),
                    None,
                )?;
            }
            Ok(policies.len())
        }
        Kind::SlaAssignment => {
            let assignments = {
                let mut stmt = tx.prepare(&format!(
                    "{ASSIGNMENT_SELECT} ORDER BY target_kind, target_id"
                ))?;
                let rows = stmt
                    .query_map([], row_to_assignment)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            for a in &assignments {
                record_core_capture_for_org_tx(
                    tx,
                    Kind::SlaAssignment,
                    &a.org_id,
                    assignment_resource_id(&a.target_kind, &a.target_id),
                    SqlWriteAction::Insert,
                    assignment_fields(a),
                    None,
                )?;
            }
            Ok(assignments.len())
        }
        other => anyhow::bail!("{other:?} is not an SLA resource"),
    }
}

/// A database with the organizations and actors the tests use: the capture
/// journal references `organizations` and `user_accounts`, so the ids must exist.
#[cfg(test)]
pub(crate) fn test_pool() -> DbPool {
    let pool = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
    {
        let conn = pool.write().unwrap();
        for org in ["org-a", "org-b"] {
            conn.execute(
                "INSERT OR IGNORE INTO organizations (org_id, name, slug, created_at) \
                 VALUES (?1, ?1, ?1, datetime('now'))",
                [org],
            )
            .unwrap();
        }
        for flow in ["flow-1", "flow-2", "flow-3"] {
            conn.execute(
                "INSERT OR IGNORE INTO flows (id, name, flow_json) VALUES (?1, ?1, '{}')",
                [flow],
            )
            .unwrap();
        }
        for user in ["a", "b", "admin", "alice", "bob", "mallory"] {
            conn.execute(
                "INSERT OR IGNORE INTO user_accounts (id, username, password_hash) \
                 VALUES (?1, 'sla-test-' || ?1, 'x')",
                [user],
            )
            .unwrap();
        }
    }
    pool
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::core_capture::{load_core_write_capture, CoreWriteCapture};
    use crate::sync::core_materializer::apply_core_operation;
    use crate::sync::ledger::{
        BaselineEpoch, HybridLogicalTimestamp, NodeEnvironment, OperationId, PartitionId,
        SyncOperationBody,
    };

    const ORG: &str = "org-a";
    const OTHER: &str = "org-b";

    fn pool() -> DbPool {
        test_pool()
    }

    fn spec(name: &str) -> SlaPolicySpec {
        SlaPolicySpec {
            name: name.to_string(),
            description: "registration".into(),
            max_concurrency: Some(2),
            max_queue: Some(4),
            queue_timeout_ms: Some(5_000),
            call_limit: Some(60),
            call_window_secs: Some(60),
            timeout_ms: Some(30_000),
            latency_p95_ms: Some(2_000),
            error_rate_pct: Some(5),
            warn_at_pct: Some(80),
            ordering: BusOrdering::None,
        }
    }

    #[test]
    fn a_policy_is_created_read_updated_and_listed_inside_its_organization() {
        let pool = pool();
        let created = create_policy(&pool, ORG, "alice", &spec("  Registration API  ")).unwrap();
        assert_eq!(created.spec.name, "Registration API", "the name is trimmed");
        assert_eq!(created.updated_by, "alice");
        assert_eq!(created.created_at_ms, created.updated_at_ms);
        assert_eq!(
            get_policy(&pool, ORG, &created.id).unwrap().as_ref(),
            Some(&created)
        );
        assert_eq!(get_policy(&pool, OTHER, &created.id).unwrap(), None);
        assert!(list_policies(&pool, OTHER).unwrap().is_empty());

        let mut edited = created.spec.clone();
        edited.max_concurrency = Some(8);
        edited.call_limit = None;
        edited.call_window_secs = None;
        edited.ordering = BusOrdering::Partition;
        let updated = update_policy(&pool, ORG, "bob", &created.id, &edited).unwrap();
        assert_eq!(updated.spec, edited);
        assert_eq!(updated.updated_by, "bob");
        assert_eq!(updated.created_at_ms, created.created_at_ms);
        assert_eq!(list_policies(&pool, ORG).unwrap(), vec![updated]);

        let missing = update_policy(&pool, OTHER, "mallory", &created.id, &edited).unwrap_err();
        assert_eq!(
            missing.code(),
            "not_found",
            "another organization's policy reads as missing"
        );
    }

    #[test]
    fn invalid_and_duplicate_policies_are_refused_with_a_code() {
        let pool = pool();
        create_policy(&pool, ORG, "a", &spec("Gold")).unwrap();
        let duplicate = create_policy(&pool, ORG, "a", &spec("Gold")).unwrap_err();
        assert_eq!(duplicate.code(), "name_taken");
        create_policy(&pool, OTHER, "a", &spec("Gold")).expect("names are per organization");

        let other = create_policy(&pool, ORG, "a", &spec("Silver")).unwrap();
        let rename = update_policy(&pool, ORG, "a", &other.id, &spec("Gold")).unwrap_err();
        assert_eq!(rename.code(), "name_taken");

        let mut bad = spec("Bad");
        bad.max_concurrency = Some(0);
        assert_eq!(
            create_policy(&pool, ORG, "a", &bad).unwrap_err().code(),
            "invalid_value"
        );
        let mut half = spec("Half");
        half.call_window_secs = None;
        assert_eq!(
            create_policy(&pool, ORG, "a", &half).unwrap_err().code(),
            "invalid_value"
        );
        let mut window = spec("Window");
        window.call_window_secs = Some(30);
        assert_eq!(
            create_policy(&pool, ORG, "a", &window).unwrap_err().code(),
            "invalid_value"
        );
        assert_eq!(
            create_policy(&pool, ORG, "a", &spec("  "))
                .unwrap_err()
                .code(),
            "invalid_value"
        );
    }

    #[test]
    fn a_policy_in_use_cannot_be_deleted_until_it_is_unassigned() {
        let pool = pool();
        let policy = create_policy(&pool, ORG, "a", &spec("Gold")).unwrap();
        assign(&pool, ORG, "a", "flow", "flow-1", &policy.id).unwrap();
        assign(&pool, ORG, "a", "flow", "flow-2", &policy.id).unwrap();

        let refused = delete_policy(&pool, ORG, "a", &policy.id).unwrap_err();
        assert_eq!(refused.code(), "policy_in_use");
        assert!(matches!(
            refused,
            SlaError::PolicyInUse { assignments: 2, .. }
        ));
        assert!(get_policy(&pool, ORG, &policy.id).unwrap().is_some());

        assert!(unassign(&pool, ORG, "a", "flow", "flow-1")
            .unwrap()
            .is_some());
        assert_eq!(
            delete_policy(&pool, ORG, "a", &policy.id)
                .unwrap_err()
                .code(),
            "policy_in_use"
        );
        assert!(unassign(&pool, ORG, "a", "flow", "flow-2")
            .unwrap()
            .is_some());
        assert!(
            unassign(&pool, ORG, "a", "flow", "flow-2")
                .unwrap()
                .is_none(),
            "nothing left to lift"
        );

        assert_eq!(
            delete_policy(&pool, OTHER, "a", &policy.id)
                .unwrap_err()
                .code(),
            "not_found"
        );
        delete_policy(&pool, ORG, "a", &policy.id).unwrap();
        assert!(get_policy(&pool, ORG, &policy.id).unwrap().is_none());
    }

    #[test]
    fn assignments_are_org_scoped_and_one_policy_per_service() {
        let pool = pool();
        let gold = create_policy(&pool, ORG, "a", &spec("Gold")).unwrap();
        let silver = create_policy(&pool, ORG, "a", &spec("Silver")).unwrap();
        let foreign = create_policy(&pool, OTHER, "a", &spec("Foreign")).unwrap();

        let first = assign(&pool, ORG, "a", "flow", "flow-1", &gold.id)
            .unwrap()
            .current;
        assert_eq!(
            (first.policy_id.as_str(), first.org_id.as_str()),
            (gold.id.as_str(), ORG)
        );
        let moved = assign(&pool, ORG, "a", "flow", "flow-1", &silver.id).unwrap();
        assert_eq!(
            moved.previous.as_ref(),
            Some(&first),
            "the replaced row is reported"
        );
        let moved = moved.current;
        assert_eq!(
            moved.policy_id, silver.id,
            "a second assignment replaces the first"
        );
        assert_eq!(list_assignments(&pool, ORG).unwrap(), vec![moved.clone()]);
        assert!(list_assignments(&pool, OTHER).unwrap().is_empty());
        assert_eq!(
            get_assignment(&pool, OTHER, "flow", "flow-1").unwrap(),
            None
        );
        assert_eq!(
            unassign(&pool, OTHER, "b", "flow", "flow-1")
                .unwrap_err()
                .code(),
            "governed_elsewhere",
            "another organization's assignment is not ours to lift"
        );
        assert!(
            unassign(&pool, OTHER, "b", "flow", "flow-2")
                .unwrap()
                .is_none(),
            "an ungoverned flow has nothing to lift"
        );

        assert_eq!(
            assign(&pool, ORG, "a", "flow", "flow-2", &foreign.id)
                .unwrap_err()
                .code(),
            "not_found"
        );
        let taken = assign(&pool, OTHER, "b", "flow", "flow-1", &foreign.id).unwrap_err();
        let missing = assign(&pool, OTHER, "b", "flow", "no-such-flow", &foreign.id).unwrap_err();
        assert_eq!(
            taken.code(),
            "governed_elsewhere",
            "a service governed by another organization is not takeable"
        );
        assert_eq!(missing.code(), "not_found");
        assert_eq!(
            assign(&pool, ORG, "a", "topic", "t", &gold.id)
                .unwrap_err()
                .code(),
            "invalid_value"
        );
        assert_eq!(
            assign(&pool, ORG, "a", "flow", " ", &gold.id)
                .unwrap_err()
                .code(),
            "invalid_value"
        );

        let active = load_active(&pool).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].target_id, "flow-1");
        assert_eq!(active[0].policy_id, silver.id);
        assert_eq!(active[0].spec, silver.spec);
    }

    fn all_captures(pool: &DbPool) -> Vec<CoreWriteCapture> {
        let conn = pool.read().unwrap();
        let ids: Vec<String> = conn
            .prepare(
                "SELECT capture_id FROM __tentaflow_core_sync_captures \
                  WHERE resource_type IN ('core.sla_policy', 'core.sla_assignment') ORDER BY rowid",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        ids.iter()
            .map(|id| load_core_write_capture(&conn, id).unwrap().unwrap())
            .collect()
    }

    fn operation_from(capture: &CoreWriteCapture) -> SyncOperation {
        SyncOperation {
            op_id: OperationId::from_hash([9; 32]),
            operation_hash: [9; 32],
            body: SyncOperationBody {
                org_id: capture.org_id.clone(),
                partition_id: PartitionId::new(format!("core/org/{}/sla", capture.org_id)).unwrap(),
                node_seq: 1,
                addon_id: crate::sync::core_registry::CORE_SYNC_ADDON_ID.to_string(),
                resource_type: capture.resource_type.clone(),
                resource_id: capture.resource_id.clone(),
                table_name: capture.table_name.clone(),
                primary_key: capture.primary_key.clone(),
                action: match capture.action {
                    SqlWriteAction::Insert => ActionType::Insert,
                    SqlWriteAction::Update => ActionType::Update,
                    SqlWriteAction::Delete => ActionType::Delete,
                },
                changed_fields: capture.changed_fields.clone(),
                before_hash: None,
                after_hash: None,
                actor_user_id: String::new(),
                actor_device_id: "peer".to_string(),
                actor_node_id: "peer".to_string(),
                hlc_timestamp: HybridLogicalTimestamp {
                    wall_time_ms: capture.hlc.wall_time_ms,
                    logical: capture.hlc.logical,
                    node_id: "peer".to_string(),
                },
                epoch: BaselineEpoch::default(),
                environment: NodeEnvironment::default(),
                prev_node_hash: None,
                payload_hash: [0; 32],
                acl_snapshot_hash: [0; 32],
                policy_epoch: 0,
                encryption_info: None,
            },
            signature: Vec::new(),
        }
    }

    #[test]
    fn the_captures_of_one_node_rebuild_the_same_state_on_a_peer() {
        let source = pool();
        let peer = pool();
        let gold = create_policy(&source, ORG, "a", &spec("Gold")).unwrap();
        let mut silver_spec = spec("Silver");
        silver_spec.max_concurrency = None;
        silver_spec.max_queue = None;
        silver_spec.call_limit = None;
        silver_spec.call_window_secs = None;
        let silver = create_policy(&source, ORG, "a", &silver_spec).unwrap();
        assign(&source, ORG, "a", "flow", "flow-1", &gold.id).unwrap();
        assign(&source, ORG, "a", "flow", "flow-2", &silver.id).unwrap();
        let mut edited = gold.spec.clone();
        edited.max_concurrency = Some(16);
        update_policy(&source, ORG, "a", &gold.id, &edited).unwrap();

        let mut replayed = 0;
        let mut replay = |source: &DbPool| {
            let captures = all_captures(source);
            for capture in &captures[replayed..] {
                let rows = apply_core_operation(&peer, &operation_from(capture)).unwrap();
                assert_eq!(rows, 1, "{} {:?}", capture.resource_type, capture.action);
            }
            replayed = captures.len();
        };
        replay(&source);
        assert_eq!(
            list_policies(&peer, ORG).unwrap(),
            list_policies(&source, ORG).unwrap()
        );
        assert_eq!(
            list_assignments(&peer, ORG).unwrap(),
            list_assignments(&source, ORG).unwrap()
        );
        assert_eq!(list_policies(&peer, ORG).unwrap().len(), 2);
        let on_peer = get_policy(&peer, ORG, &gold.id).unwrap().unwrap();
        assert_eq!(on_peer.spec.max_concurrency, Some(16));
        assert_eq!(on_peer.spec.ordering, BusOrdering::None);
        let silver_on_peer = get_policy(&peer, ORG, &silver.id).unwrap().unwrap();
        assert_eq!(
            silver_on_peer.spec.call_limit, None,
            "an unset limit stays unset"
        );

        unassign(&source, ORG, "a", "flow", "flow-1").unwrap();
        delete_policy(&source, ORG, "a", &gold.id).unwrap();
        replay(&source);
        assert_eq!(list_policies(&peer, ORG).unwrap(), vec![silver_on_peer]);
        assert_eq!(list_assignments(&peer, ORG).unwrap().len(), 1);
    }

    #[test]
    fn applying_from_sync_moves_the_generation() {
        let source = pool();
        let peer = pool();
        let policy = create_policy(&source, ORG, "a", &spec("Gold")).unwrap();
        assign(&source, ORG, "a", "flow", "flow-1", &policy.id).unwrap();
        let captures = all_captures(&source);
        assert_eq!(captures.len(), 2);

        let mut seen = crate::sla::generation();
        for capture in &captures {
            apply_core_operation(&peer, &operation_from(capture)).unwrap();
            let now = crate::sla::generation();
            assert!(
                now > seen,
                "{} did not bump the generation",
                capture.resource_type
            );
            seen = now;
        }
        let registry = crate::sla::SlaRegistry::default();
        assert!(registry.refresh_if_stale(&peer).unwrap());
        assert_eq!(
            registry.gate("flow-1").unwrap().config().policy_id,
            policy.id
        );
    }

    #[test]
    fn an_assignment_that_arrives_before_its_policy_is_deferred() {
        let source = pool();
        let peer = pool();
        let policy = create_policy(&source, ORG, "a", &spec("Gold")).unwrap();
        assign(&source, ORG, "a", "flow", "flow-1", &policy.id).unwrap();
        let captures = all_captures(&source);

        let early = apply_core_operation(&peer, &operation_from(&captures[1])).unwrap_err();
        assert!(
            matches!(early, SyncLedgerError::DeferredOrdering(_)),
            "{early:?}"
        );
        apply_core_operation(&peer, &operation_from(&captures[0])).unwrap();
        apply_core_operation(&peer, &operation_from(&captures[1])).unwrap();
        assert_eq!(list_assignments(&peer, ORG).unwrap().len(), 1);
    }

    #[test]
    fn a_peer_cannot_write_into_another_organization() {
        let source = pool();
        let peer = pool();
        let mine = create_policy(&source, ORG, "a", &spec("Gold")).unwrap();
        let capture = all_captures(&source).remove(0);
        apply_core_operation(&peer, &operation_from(&capture)).unwrap();

        // The row says org-a, the envelope says org-b.
        let mut forged = operation_from(&capture);
        forged.body.org_id = OTHER.to_string();
        forged.body.hlc_timestamp.wall_time_ms += 5;
        assert!(apply_core_operation(&peer, &forged).is_err());

        // org-b's own envelope and row, aimed at org-a's id: nothing changes.
        let mut hijack = operation_from(&capture);
        hijack.body.org_id = OTHER.to_string();
        hijack
            .body
            .changed_fields
            .insert("org_id".into(), FieldValue::String(OTHER.into()));
        hijack
            .body
            .changed_fields
            .insert("name".into(), FieldValue::String("Hijacked".into()));
        hijack.body.hlc_timestamp.wall_time_ms += 10;
        assert_eq!(apply_core_operation(&peer, &hijack).unwrap(), 0);

        // A delete under the wrong envelope reaches nothing either.
        let mut foreign_delete = hijack.clone();
        foreign_delete.body.action = ActionType::Delete;
        foreign_delete.body.changed_fields.clear();
        foreign_delete.body.hlc_timestamp.wall_time_ms += 10;
        assert_eq!(apply_core_operation(&peer, &foreign_delete).unwrap(), 0);

        let kept = get_policy(&peer, ORG, &mine.id)
            .unwrap()
            .expect("still org-a's");
        assert_eq!(kept.spec.name, "Gold");
        assert!(list_policies(&peer, OTHER).unwrap().is_empty());
    }

    #[test]
    fn a_policy_that_arrives_with_a_taken_name_does_not_block_the_stream() {
        let source = pool();
        let peer = pool();
        let remote = create_policy(&source, ORG, "a", &spec("Gold")).unwrap();
        let local = create_policy(&peer, ORG, "a", &spec("Gold")).unwrap();
        set_created(&source, &remote.id, 100);
        set_created(&peer, &local.id, 200);
        let operation = policy_operation(&source, 100);
        assert_eq!(apply_core_operation(&peer, &operation).unwrap(), 1);

        let policies = list_policies(&peer, ORG).unwrap();
        assert_eq!(
            policies.len(),
            2,
            "the stream is not blocked by the name clash"
        );
        // `remote` is the older one, so it keeps the plain name and the policy
        // that was already here is the one that gets told apart.
        let arrived = policies.iter().find(|p| p.id == remote.id).unwrap();
        assert_eq!(arrived.spec.name, "Gold");
        let renamed = policies.iter().find(|p| p.id == local.id).unwrap();
        assert!(
            renamed.spec.name.starts_with("Gold ("),
            "{}",
            renamed.spec.name
        );
    }

    #[test]
    fn the_baseline_reseed_captures_every_stored_policy_and_assignment() {
        let pool = pool();
        let policy = create_policy(&pool, ORG, "a", &spec("Gold")).unwrap();
        assign(&pool, ORG, "a", "flow", "flow-1", &policy.id).unwrap();
        let before = all_captures(&pool).len();

        crate::db::repository::reseed_core_state_from_current_rows(&pool).unwrap();
        let captures = all_captures(&pool);
        assert_eq!(captures.len(), before + 2);
        let reseeded = &captures[before..];
        assert_eq!(reseeded[0].resource_type, "core.sla_policy");
        assert_eq!(reseeded[0].resource_id, policy.id);
        assert_eq!(reseeded[0].org_id, ORG);
        assert_eq!(reseeded[1].resource_type, "core.sla_assignment");
        assert_eq!(reseeded[0].action, SqlWriteAction::Insert);

        let peer = self::pool();
        for capture in reseeded {
            apply_core_operation(&peer, &operation_from(capture)).unwrap();
        }
        assert_eq!(
            list_policies(&peer, ORG).unwrap(),
            list_policies(&pool, ORG).unwrap()
        );
        assert_eq!(
            list_assignments(&peer, ORG).unwrap(),
            list_assignments(&pool, ORG).unwrap()
        );
    }

    fn set_created(pool: &DbPool, id: &str, created_at_ms: i64) {
        pool.write()
            .unwrap()
            .execute(
                "UPDATE service_sla_policies SET created_at_ms = ?2 WHERE id = ?1",
                rusqlite::params![id, created_at_ms],
            )
            .unwrap();
    }

    fn policy_operation(source: &DbPool, created_at_ms: i64) -> SyncOperation {
        let mut operation = operation_from(&all_captures(source).remove(0));
        operation
            .body
            .changed_fields
            .insert("created_at_ms".into(), FieldValue::I64(created_at_ms));
        operation
    }

    fn names(pool: &DbPool) -> BTreeMap<String, String> {
        list_policies(pool, ORG)
            .unwrap()
            .into_iter()
            .map(|p| (p.id, p.spec.name))
            .collect()
    }

    /// Two nodes that each created "Gold" before seeing the other end up with
    /// the same names whichever of the two is older.
    #[test]
    fn a_name_clash_is_settled_the_same_on_both_nodes_in_both_directions() {
        for (first_created, second_created) in [(100, 200), (200, 100), (150, 150)] {
            let (n1, n2) = (pool(), pool());
            let p1 = create_policy(&n1, ORG, "a", &spec("Gold")).unwrap();
            let p2 = create_policy(&n2, ORG, "a", &spec("Gold")).unwrap();
            set_created(&n1, &p1.id, first_created);
            set_created(&n2, &p2.id, second_created);
            let (op1, op2) = (
                policy_operation(&n1, first_created),
                policy_operation(&n2, second_created),
            );
            apply_core_operation(&n1, &op2).unwrap();
            apply_core_operation(&n2, &op1).unwrap();

            assert_eq!(
                names(&n1),
                names(&n2),
                "created {first_created}/{second_created}"
            );
            let settled = names(&n1);
            assert_eq!(settled.len(), 2);
            let first_wins = (first_created, p1.id.as_str()) < (second_created, p2.id.as_str());
            let (winner, loser) = if first_wins {
                (&p1.id, &p2.id)
            } else {
                (&p2.id, &p1.id)
            };
            assert_eq!(
                settled[winner], "Gold",
                "the older policy keeps the plain name"
            );
            assert!(settled[loser].starts_with("Gold ("), "{}", settled[loser]);
            assert!(settled[loser].len() <= MAX_NAME_CHARS);
        }
    }

    #[test]
    fn the_suffix_of_a_clashing_name_is_the_random_tail_of_the_id() {
        let id = "01a1e53f-9f79-7640-9174-b7d21a71862e";
        assert_eq!(distinguished_name("Gold", id), "Gold (1a71862e)");
    }

    #[test]
    fn three_policies_with_one_name_created_in_the_same_minute_converge_in_any_order() {
        let nodes = [pool(), pool(), pool()];
        let policies: Vec<_> = nodes
            .iter()
            .map(|n| create_policy(n, ORG, "a", &spec("Gold")).unwrap())
            .collect();
        for (node, policy) in nodes.iter().zip(&policies) {
            set_created(node, &policy.id, 1_000);
        }
        let operations: Vec<_> = nodes.iter().map(|n| policy_operation(n, 1_000)).collect();
        // Every node gets the other two, each in a different order.
        for (index, node) in nodes.iter().enumerate() {
            let mut others: Vec<usize> = (0..3).filter(|i| *i != index).collect();
            if index % 2 == 1 {
                others.reverse();
            }
            for other in others {
                apply_core_operation(node, &operations[other]).unwrap();
            }
        }
        assert_eq!(names(&nodes[0]), names(&nodes[1]));
        assert_eq!(names(&nodes[1]), names(&nodes[2]));
        let settled = names(&nodes[0]);
        assert_eq!(settled.len(), 3);
        assert_eq!(settled.values().filter(|n| *n == "Gold").count(), 1);
        let distinct: std::collections::BTreeSet<_> = settled.values().collect();
        assert_eq!(distinct.len(), 3, "no two share a name");
        let smallest = policies.iter().map(|p| &p.id).min().unwrap();
        assert_eq!(
            settled[smallest], "Gold",
            "the tie on the time goes to the smaller id"
        );
    }

    #[test]
    fn a_clash_on_a_name_of_195_or_more_characters_still_fits_the_bound() {
        for length in [195, 199, 200] {
            let long = "x".repeat(length);
            let (n1, n2) = (pool(), pool());
            let p1 = create_policy(&n1, ORG, "a", &spec(&long)).unwrap();
            let p2 = create_policy(&n2, ORG, "a", &spec(&long)).unwrap();
            set_created(&n1, &p1.id, 10);
            set_created(&n2, &p2.id, 20);
            apply_core_operation(&n1, &policy_operation(&n2, 20)).unwrap();
            apply_core_operation(&n2, &policy_operation(&n1, 10)).unwrap();
            assert_eq!(names(&n1), names(&n2), "length {length}");
            let settled = names(&n1);
            assert_eq!(settled.len(), 2);
            assert!(settled
                .values()
                .all(|n| n.chars().count() <= MAX_NAME_CHARS));
            assert_eq!(settled.values().filter(|n| **n == long).count(), 1);
        }
    }

    #[test]
    fn a_flow_that_does_not_exist_or_is_a_system_flow_cannot_be_assigned() {
        let pool = pool();
        let policy = create_policy(&pool, ORG, "a", &spec("Gold")).unwrap();
        assert_eq!(
            assign(&pool, ORG, "a", "flow", "no-such-flow", &policy.id)
                .unwrap_err()
                .code(),
            "not_found"
        );
        pool.write()
            .unwrap()
            .execute("UPDATE flows SET is_system = 1 WHERE id = 'flow-3'", [])
            .unwrap();
        assert_eq!(
            assign(&pool, ORG, "a", "flow", "flow-3", &policy.id)
                .unwrap_err()
                .code(),
            "system_flow"
        );
        assign(&pool, ORG, "a", "flow", "flow-1", &policy.id).unwrap();
    }

    #[test]
    fn an_assignment_of_one_flow_by_two_organizations_converges_on_the_later_write() {
        let (n1, n2) = (pool(), pool());
        let gold = create_policy(&n1, ORG, "a", &spec("Gold")).unwrap();
        let other = create_policy(&n2, OTHER, "b", &spec("Other")).unwrap();
        let (gold_op, other_op) = (
            operation_from(&all_captures(&n1).remove(0)),
            operation_from(&all_captures(&n2).remove(0)),
        );
        apply_core_operation(&n1, &other_op).unwrap();
        apply_core_operation(&n2, &gold_op).unwrap();

        assign(&n1, ORG, "a", "flow", "flow-1", &gold.id).unwrap();
        assign(&n2, OTHER, "b", "flow", "flow-1", &other.id).unwrap();
        let from_n1 = operation_from(all_captures(&n1).last().unwrap());
        let from_n2 = operation_from(all_captures(&n2).last().unwrap());
        apply_core_operation(&n1, &from_n2).unwrap();
        apply_core_operation(&n2, &from_n1).unwrap();

        for node in [&n1, &n2] {
            let held = get_assignment(node, OTHER, "flow", "flow-1").unwrap();
            assert_eq!(held.map(|a| a.policy_id), Some(other.id.clone()));
            assert_eq!(get_assignment(node, ORG, "flow", "flow-1").unwrap(), None);
        }
    }

    #[test]
    fn an_assignment_that_lost_a_race_with_its_policys_delete_is_dropped_not_deferred() {
        let source = pool();
        let peer = pool();
        let policy = create_policy(&source, ORG, "a", &spec("Gold")).unwrap();
        assign(&source, ORG, "a", "flow", "flow-1", &policy.id).unwrap();
        unassign(&source, ORG, "a", "flow", "flow-1").unwrap();
        delete_policy(&source, ORG, "a", &policy.id).unwrap();
        let ops: Vec<_> = all_captures(&source).iter().map(operation_from).collect();
        assert_eq!(ops.len(), 4, "policy, assign, unassign, delete");

        apply_core_operation(&peer, &ops[0]).unwrap();
        apply_core_operation(&peer, &ops[3]).unwrap();
        assert!(list_policies(&peer, ORG).unwrap().is_empty());
        assert_eq!(apply_core_operation(&peer, &ops[1]).unwrap(), 0);
        assert!(list_assignments(&peer, ORG).unwrap().is_empty());
    }

    #[test]
    fn a_replicated_policy_delete_removes_assignments_still_pointing_at_it() {
        let source = pool();
        let peer = pool();
        let policy = create_policy(&source, ORG, "a", &spec("Gold")).unwrap();
        assign(&source, ORG, "a", "flow", "flow-1", &policy.id).unwrap();
        unassign(&source, ORG, "a", "flow", "flow-1").unwrap();
        delete_policy(&source, ORG, "a", &policy.id).unwrap();
        let ops: Vec<_> = all_captures(&source).iter().map(operation_from).collect();
        apply_core_operation(&peer, &ops[0]).unwrap();
        apply_core_operation(&peer, &ops[1]).unwrap();
        assert_eq!(list_assignments(&peer, ORG).unwrap().len(), 1);
        // The unassign has not arrived yet, the delete has.
        assert_eq!(apply_core_operation(&peer, &ops[3]).unwrap(), 1);
        assert!(list_assignments(&peer, ORG).unwrap().is_empty());
        assert!(list_policies(&peer, ORG).unwrap().is_empty());
    }

    #[test]
    fn unassign_records_its_actor() {
        let pool = pool();
        let policy = create_policy(&pool, ORG, "a", &spec("Gold")).unwrap();
        assign(&pool, ORG, "a", "flow", "flow-1", &policy.id).unwrap();
        unassign(&pool, ORG, "bob", "flow", "flow-1").unwrap();
        let last = all_captures(&pool).pop().unwrap();
        assert_eq!(last.action, SqlWriteAction::Delete);
        assert_eq!(last.actor_user_id.as_deref(), Some("bob"));
    }

    #[test]
    fn a_description_above_2000_characters_is_refused() {
        let pool = pool();
        let mut ok = spec("Long");
        ok.description = "d".repeat(2000);
        create_policy(&pool, ORG, "a", &ok).unwrap();
        let mut bad = spec("Too long");
        bad.description = "d".repeat(2001);
        assert_eq!(
            create_policy(&pool, ORG, "a", &bad).unwrap_err().code(),
            "invalid_value"
        );
    }
}
