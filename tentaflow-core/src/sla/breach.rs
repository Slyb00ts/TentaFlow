//! Storage of SLA breaches and minute samples (tables of migration 212) and the
//! org-pinned reads the protocol layer needs.
//!
//! Both tables are node-local: what a node measured about a service is that
//! node's own fact, so nothing here enters the Sync Ledger. A breach is one row
//! for as long as it lasts. The partial unique index on `dedupe_key` (only
//! while `resolved_at_ms IS NULL`) is what keeps a breach that is still open
//! from being raised a second time, while a resolved one frees its key; the
//! raise / refresh / resolve functions follow the pattern of
//! `tentanas::db::raise_coded_alert` / `resolve_alert` (read first, write only
//! when something changes).
//!
//! A breach is coded: `kind` is a stable code and `threshold` / `peak_value`
//! are its parameters, so the UI words it in the user's language. The unit of
//! the two numbers follows the kind, see [`BreachKind::unit`].

use std::collections::HashMap;

use rusqlite::{params, OptionalExtension};

use super::repository::Result;
use crate::db::DbPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BreachKind {
    /// Call-window usage reached `warn_at_pct` of the limit.
    CallsWarn,
    /// Call-window usage reached the limit.
    CallsLimit,
    /// Every concurrency slot was taken for at least a minute.
    ConcurrencySaturated,
    /// p95 latency over the evaluation window exceeded the policy.
    Latency,
    /// Error rate over the evaluation window reached the policy.
    Errors,
    /// At least one call hit the execution timeout in the window.
    Timeouts,
}

/// What a breach's `threshold` and `peak_value` are measured in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreachUnit {
    Milliseconds,
    Percent,
    Calls,
}

impl BreachKind {
    pub const ALL: [BreachKind; 6] = [
        Self::CallsWarn,
        Self::CallsLimit,
        Self::ConcurrencySaturated,
        Self::Latency,
        Self::Errors,
        Self::Timeouts,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CallsWarn => "calls_warn",
            Self::CallsLimit => "calls_limit",
            Self::ConcurrencySaturated => "concurrency_saturated",
            Self::Latency => "latency",
            Self::Errors => "errors",
            Self::Timeouts => "timeouts",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// `calls_warn` and `errors` are percentages, `latency` milliseconds, the
    /// rest are counts (calls, slots, timed-out calls).
    pub fn unit(self) -> BreachUnit {
        match self {
            Self::CallsWarn | Self::Errors => BreachUnit::Percent,
            Self::Latency => BreachUnit::Milliseconds,
            Self::CallsLimit | Self::ConcurrencySaturated | Self::Timeouts => BreachUnit::Calls,
        }
    }
}

/// The key that makes a breach unique while it is open.
pub fn dedupe_key(org_id: &str, target_id: &str, kind: BreachKind) -> String {
    format!("{org_id}:{target_id}:{}", kind.as_str())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Breach {
    pub id: String,
    pub org_id: String,
    pub target_id: String,
    pub policy_id: String,
    pub kind: BreachKind,
    pub node_id: String,
    pub threshold: f64,
    pub peak_value: f64,
    pub raised_at_ms: i64,
    pub resolved_at_ms: Option<i64>,
}

impl Breach {
    pub fn is_open(&self) -> bool {
        self.resolved_at_ms.is_none()
    }

    pub fn dedupe_key(&self) -> String {
        dedupe_key(&self.org_id, &self.target_id, self.kind)
    }
}

/// A condition the evaluator found to hold.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub org_id: String,
    pub target_id: String,
    pub policy_id: String,
    pub kind: BreachKind,
    pub threshold: f64,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RaiseOutcome {
    /// A new breach row was opened.
    Raised(Breach),
    /// The open breach saw a higher value (or a changed threshold or policy).
    Refreshed,
    /// The open breach already says exactly this.
    Unchanged,
}

const BREACH_SELECT: &str = "SELECT id, org_id, target_id, policy_id, kind, node_id, threshold, \
     peak_value, raised_at_ms, resolved_at_ms FROM sla_breaches";

fn row_to_breach(r: &rusqlite::Row<'_>) -> rusqlite::Result<Breach> {
    let kind: String = r.get(4)?;
    Ok(Breach {
        id: r.get(0)?,
        org_id: r.get(1)?,
        target_id: r.get(2)?,
        policy_id: r.get(3)?,
        // The column is CHECK-ed to the six codes.
        kind: BreachKind::parse(&kind).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                format!("unknown breach kind {kind}").into(),
            )
        })?,
        node_id: r.get(5)?,
        threshold: r.get(6)?,
        peak_value: r.get(7)?,
        raised_at_ms: r.get(8)?,
        resolved_at_ms: r.get(9)?,
    })
}

/// Opens the breach, or refreshes the one that is open: its `peak_value` only
/// ever rises while it lasts, and a changed threshold or policy is carried over
/// so the row describes the policy that is in force now. Reads first, so an
/// open breach that says exactly this costs no write.
pub fn raise_or_refresh(
    pool: &DbPool,
    candidate: &Candidate,
    node_id: &str,
    now_ms: i64,
) -> Result<RaiseOutcome> {
    let key = dedupe_key(&candidate.org_id, &candidate.target_id, candidate.kind);
    let open: Option<(String, f64, f64, String)> = {
        let conn = pool.read()?;
        conn.query_row(
            "SELECT id, peak_value, threshold, policy_id FROM sla_breaches \
             WHERE dedupe_key = ?1 AND resolved_at_ms IS NULL",
            [&key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
    };
    let conn = pool.write()?;
    if let Some((id, peak, threshold, policy_id)) = open {
        let unchanged = candidate.value <= peak
            && candidate.threshold == threshold
            && candidate.policy_id == policy_id;
        if unchanged {
            return Ok(RaiseOutcome::Unchanged);
        }
        conn.execute(
            "UPDATE sla_breaches SET peak_value = MAX(peak_value, ?2), threshold = ?3, \
             policy_id = ?4 WHERE id = ?1 AND resolved_at_ms IS NULL",
            params![
                id,
                candidate.value,
                candidate.threshold,
                candidate.policy_id
            ],
        )?;
        return Ok(RaiseOutcome::Refreshed);
    }
    let id = uuid::Uuid::now_v7().to_string();
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO sla_breaches \
            (id, org_id, target_id, policy_id, kind, node_id, threshold, peak_value, \
             raised_at_ms, resolved_at_ms, dedupe_key) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10)",
        params![
            id,
            candidate.org_id,
            candidate.target_id,
            candidate.policy_id,
            candidate.kind.as_str(),
            node_id,
            candidate.threshold,
            candidate.value,
            now_ms,
            key,
        ],
    )?;
    if inserted == 0 {
        return Ok(RaiseOutcome::Unchanged);
    }
    Ok(RaiseOutcome::Raised(Breach {
        id,
        org_id: candidate.org_id.clone(),
        target_id: candidate.target_id.clone(),
        policy_id: candidate.policy_id.clone(),
        kind: candidate.kind,
        node_id: node_id.to_string(),
        threshold: candidate.threshold,
        peak_value: candidate.value,
        raised_at_ms: now_ms,
        resolved_at_ms: None,
    }))
}

/// Closes an open breach. `true` when a row was closed, so the caller audits
/// the resolution once.
pub fn resolve(pool: &DbPool, breach_id: &str, now_ms: i64) -> Result<bool> {
    let conn = pool.write()?;
    let closed = conn.execute(
        "UPDATE sla_breaches SET resolved_at_ms = ?2 WHERE id = ?1 AND resolved_at_ms IS NULL",
        params![breach_id, now_ms],
    )?;
    Ok(closed == 1)
}

/// Every breach this node holds open, of every organization: the evaluator
/// works for the node, not for one org.
pub fn open_breaches(pool: &DbPool) -> Result<Vec<Breach>> {
    let conn = pool.read()?;
    let mut stmt = conn.prepare(&format!("{BREACH_SELECT} WHERE resolved_at_ms IS NULL"))?;
    let rows = stmt
        .query_map([], row_to_breach)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Every open breach of one organization, optionally of one service. Not
/// paged: a status read needs all of them, and an open breach is bounded by
/// (services x kinds).
pub fn open_breaches_of_org(
    pool: &DbPool,
    org_id: &str,
    target_id: Option<&str>,
) -> Result<Vec<Breach>> {
    let conn = pool.read()?;
    let mut stmt = conn.prepare(&format!(
        "{BREACH_SELECT} WHERE org_id = ?1 AND (?2 IS NULL OR target_id = ?2) \
           AND resolved_at_ms IS NULL ORDER BY raised_at_ms DESC, id DESC"
    ))?;
    let rows = stmt
        .query_map(params![org_id, target_id], row_to_breach)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Every open breach of the organizations other than `org_id`.
pub fn open_breaches_outside_org(pool: &DbPool, org_id: &str) -> Result<Vec<Breach>> {
    let conn = pool.read()?;
    let mut stmt = conn.prepare(&format!(
        "{BREACH_SELECT} WHERE org_id <> ?1 AND resolved_at_ms IS NULL \
         ORDER BY raised_at_ms DESC, id DESC"
    ))?;
    let rows = stmt
        .query_map(params![org_id], row_to_breach)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// How many breaches are open on this node, for the metrics export.
pub fn count_open(pool: &DbPool) -> Result<u64> {
    let conn = pool.read()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sla_breaches WHERE resolved_at_ms IS NULL",
        [],
        |r| r.get(0),
    )?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Largest page a caller can ask for.
pub const MAX_PAGE_SIZE: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreachQuery {
    /// Only breaches that are still open.
    pub open_only: bool,
    pub offset: u32,
    /// Clamped to `1..=MAX_PAGE_SIZE`.
    pub limit: u32,
}

impl Default for BreachQuery {
    fn default() -> Self {
        Self {
            open_only: false,
            offset: 0,
            limit: 50,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BreachPage {
    /// Newest first.
    pub items: Vec<Breach>,
    /// Rows the filter matches, whatever the page.
    pub total: u64,
}

/// The breaches of one organization, newest first. A breach of another
/// organization is never returned; `target_id` narrows to one service.
pub fn list_breaches(
    pool: &DbPool,
    org_id: &str,
    target_id: Option<&str>,
    query: BreachQuery,
) -> Result<BreachPage> {
    let limit = query.limit.clamp(1, MAX_PAGE_SIZE);
    let conn = pool.read()?;
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sla_breaches \
         WHERE org_id = ?1 AND (?2 IS NULL OR target_id = ?2) \
           AND (?3 = 0 OR resolved_at_ms IS NULL)",
        params![org_id, target_id, query.open_only],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "{BREACH_SELECT} WHERE org_id = ?1 AND (?2 IS NULL OR target_id = ?2) \
           AND (?3 = 0 OR resolved_at_ms IS NULL) \
         ORDER BY raised_at_ms DESC, id DESC LIMIT ?4 OFFSET ?5"
    ))?;
    let items = stmt
        .query_map(
            params![org_id, target_id, query.open_only, limit, query.offset],
            row_to_breach,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(BreachPage {
        items,
        total: u64::try_from(total).unwrap_or(0),
    })
}

/// One minute of one service, as stored in `sla_samples`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub org_id: String,
    pub target_id: String,
    /// Start of the minute the sample is stamped with.
    pub sampled_at_ms: i64,
    pub accepted: u64,
    pub rejected_concurrency: u64,
    pub rejected_calls: u64,
    pub timeouts: u64,
    pub errors: u64,
    /// The most calls in flight that an observation saw during the minute.
    pub in_flight_peak: u64,
    /// p95 latency of the calls that finished in the minute; `None` when none did.
    pub p95_ms: Option<u64>,
}

fn to_sql(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// `INSERT OR IGNORE`: the minute is written once. A retried round is
/// idempotent, and a wall clock that stepped back cannot overwrite a minute that
/// was already stored with a later one's data.
pub fn write_sample(pool: &DbPool, sample: &Sample) -> Result<()> {
    let conn = pool.write()?;
    conn.execute(
        "INSERT OR IGNORE INTO sla_samples \
            (org_id, target_id, sampled_at_ms, accepted, rejected_concurrency, rejected_calls, \
             timeouts, errors, in_flight_peak, p95_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            sample.org_id,
            sample.target_id,
            sample.sampled_at_ms,
            to_sql(sample.accepted),
            to_sql(sample.rejected_concurrency),
            to_sql(sample.rejected_calls),
            to_sql(sample.timeouts),
            to_sql(sample.errors),
            to_sql(sample.in_flight_peak),
            sample.p95_ms.map(to_sql),
        ],
    )?;
    Ok(())
}

/// The minute samples of one service of one organization since `since_ms`,
/// oldest first.
pub fn list_samples(
    pool: &DbPool,
    org_id: &str,
    target_id: &str,
    since_ms: i64,
) -> Result<Vec<Sample>> {
    let conn = pool.read()?;
    let mut stmt = conn.prepare(
        "SELECT org_id, target_id, sampled_at_ms, accepted, rejected_concurrency, rejected_calls, \
                timeouts, errors, in_flight_peak, p95_ms \
           FROM sla_samples \
          WHERE org_id = ?1 AND target_id = ?2 AND sampled_at_ms >= ?3 \
          ORDER BY sampled_at_ms",
    )?;
    let from_sql = |v: i64| u64::try_from(v).unwrap_or(0);
    let rows = stmt
        .query_map(params![org_id, target_id, since_ms], |r| {
            Ok(Sample {
                org_id: r.get(0)?,
                target_id: r.get(1)?,
                sampled_at_ms: r.get(2)?,
                accepted: from_sql(r.get(3)?),
                rejected_concurrency: from_sql(r.get(4)?),
                rejected_calls: from_sql(r.get(5)?),
                timeouts: from_sql(r.get(6)?),
                errors: from_sql(r.get(7)?),
                in_flight_peak: from_sql(r.get(8)?),
                p95_ms: r.get::<_, Option<i64>>(9)?.map(from_sql),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Which organizations' samples a read covers.
#[derive(Debug, Clone, Copy)]
pub enum OrgScope<'a> {
    Only(&'a str),
    AllBut(&'a str),
}

/// Refusals stored in the samples stamped `since_ms` or later, as
/// `(rejected_calls, rejected_concurrency)` per service: one query for every
/// service in scope. A service named in `watermarks` is counted only up to its
/// watermark (inclusive): what the evaluator flushed after the watermark was
/// read is still in the gate's unflushed part and must not be counted twice.
pub fn rejections_since(
    pool: &DbPool,
    scope: OrgScope<'_>,
    since_ms: i64,
    watermarks: &HashMap<String, i64>,
) -> Result<HashMap<String, (u64, u64)>> {
    let (op, org) = match scope {
        OrgScope::Only(org) => ("=", org),
        OrgScope::AllBut(org) => ("<>", org),
    };
    let mut values: Vec<rusqlite::types::Value> = vec![org.to_string().into(), since_ms.into()];
    let mut cap = String::new();
    for (target, watermark) in watermarks {
        values.push(target.clone().into());
        values.push((*watermark).into());
        let n = values.len();
        cap.push_str(&format!(" WHEN ?{} THEN ?{}", n - 1, n));
    }
    let upper = if cap.is_empty() {
        String::new()
    } else {
        format!(" AND sampled_at_ms <= CASE target_id{cap} ELSE 9223372036854775807 END")
    };
    let conn = pool.read()?;
    let mut stmt = conn.prepare(&format!(
        "SELECT target_id, SUM(rejected_calls), SUM(rejected_concurrency) \
           FROM sla_samples \
          WHERE org_id {op} ?1 AND sampled_at_ms >= ?2{upper} \
          GROUP BY target_id"
    ))?;
    let from_sql = |v: i64| u64::try_from(v).unwrap_or(0);
    let rows = stmt
        .query_map(rusqlite::params_from_iter(values), |r| {
            Ok((
                r.get::<_, String>(0)?,
                (from_sql(r.get(1)?), from_sql(r.get(2)?)),
            ))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(rows)
}

/// What a prune removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pruned {
    pub samples: usize,
    pub breaches: usize,
}

/// Deletes samples older than `samples_before_ms` and breaches that were
/// resolved before `breaches_before_ms`. An open breach is never pruned,
/// however old.
pub fn prune(pool: &DbPool, samples_before_ms: i64, breaches_before_ms: i64) -> Result<Pruned> {
    let conn = pool.write()?;
    let samples = conn.execute(
        "DELETE FROM sla_samples WHERE sampled_at_ms < ?1",
        [samples_before_ms],
    )?;
    let breaches = conn.execute(
        "DELETE FROM sla_breaches WHERE resolved_at_ms IS NOT NULL AND resolved_at_ms < ?1",
        [breaches_before_ms],
    )?;
    Ok(Pruned { samples, breaches })
}
