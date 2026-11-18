//! The current state of a governed service, as the protocol layer shows it:
//! what is in flight against the concurrency limit, how much of the call
//! window is used and when it resets, what the last evaluation window saw
//! (p95, error rate, rejections) and which breaches are open.
//!
//! Reads are pinned to one organization: an assignment of another one reads as
//! missing, so the caller answers `not_found`.

use std::collections::HashMap;

use super::breach::{self, Breach, BreachKind, OrgScope};
use super::evaluator::{Flushed, WindowSummary, WindowViews};
use super::policy::{SlaAssignment, SLA_TARGET_FLOW};
use super::registry::SlaRegistry;
use super::repository::{self, Result, SlaError};
use crate::db::DbPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallWindowStatus {
    /// Calls counted against the limit in this window: what decides a refusal.
    pub used: u32,
    /// Of those, the calls that actually started (a reservation a bus reactor
    /// gave back is in `used` for a moment and not here); what the breach
    /// judging reads.
    pub ran: u32,
    pub limit: u32,
    pub window_secs: u32,
    pub resets_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServiceStatus {
    pub target_id: String,
    pub policy_id: String,
    /// Calls running now (see `GateStats::running`).
    pub in_flight: u64,
    /// Calls waiting in the queue for a free slot.
    pub waiting: u64,
    pub max_concurrency: Option<u32>,
    pub call_window: Option<CallWindowStatus>,
    /// `None` until the evaluator has looked at the service.
    pub window: Option<WindowSummary>,
    /// Refusals since this node started, per reason.
    pub rejected_calls_total: u64,
    pub rejected_concurrency_total: u64,
    /// Refusals since local midnight on this node: the stored minute samples
    /// plus the part of the current minute no sample holds yet. A restart
    /// loses only that unsampled part.
    ///
    /// Skew of up to about 30 s at the day boundary: a sample is written on the
    /// first tick of a minute and stamped with the minute before it, so one
    /// written just after midnight can carry the previous minute's stamp and
    /// fall into yesterday, and the unsampled part can still hold refusals from
    /// before midnight. The stamping is deliberate (S6) and is not changed.
    pub rejected_calls_today: u64,
    pub rejected_concurrency_today: u64,
    pub open_breaches: Vec<Breach>,
}

/// How a governed service stands, judged by its open breaches only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Ok,
    /// Only the call-window warning is open: the limit is near, not reached.
    Warn,
    /// Any other breach is open.
    Breach,
}

impl Health {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Breach => "breach",
        }
    }
}

impl ServiceStatus {
    pub fn health(&self) -> Health {
        health_of(&self.open_breaches)
    }
}

pub fn health_of(open_breaches: &[Breach]) -> Health {
    if open_breaches
        .iter()
        .any(|breach| breach.kind != BreachKind::CallsWarn)
    {
        Health::Breach
    } else if open_breaches.is_empty() {
        Health::Ok
    } else {
        Health::Warn
    }
}

/// The stored refusals since `day_start_ms` and the flush state they are
/// counted against. The flush state is read first and bounds the samples read
/// after it, so a flush that lands in between is counted once: its sample is
/// past the watermark and its refusals are still in the gate's unflushed part.
fn today_so_far(
    pool: &DbPool,
    views: &WindowViews,
    scope: OrgScope<'_>,
    day_start_ms: i64,
) -> Result<(HashMap<String, (u64, u64)>, HashMap<String, Flushed>)> {
    let flushed = views.flushed_snapshot();
    let watermarks: HashMap<String, i64> = flushed
        .iter()
        .map(|(target, f)| (target.clone(), f.watermark_ms))
        .collect();
    let sampled = breach::rejections_since(pool, scope, day_start_ms, &watermarks)?;
    Ok((sampled, flushed))
}

fn status_of(
    registry: &SlaRegistry,
    views: &WindowViews,
    assignment: &SlaAssignment,
    open_breaches: Vec<Breach>,
    sampled_today: &HashMap<String, (u64, u64)>,
    flushed: &HashMap<String, Flushed>,
) -> ServiceStatus {
    let mut status = ServiceStatus {
        target_id: assignment.target_id.clone(),
        policy_id: assignment.policy_id.clone(),
        in_flight: 0,
        waiting: 0,
        max_concurrency: None,
        call_window: None,
        window: views.get(&assignment.target_id),
        rejected_calls_total: 0,
        rejected_concurrency_total: 0,
        rejected_calls_today: 0,
        rejected_concurrency_today: 0,
        open_breaches,
    };
    let (sampled_calls, sampled_concurrency) = sampled_today
        .get(&assignment.target_id)
        .copied()
        .unwrap_or_default();
    status.rejected_calls_today = sampled_calls;
    status.rejected_concurrency_today = sampled_concurrency;
    // Without a gate (the registry has not loaded the assignment yet) the
    // service is reported idle rather than missing: the assignment exists.
    if let Some(gate) = registry.gate(&assignment.target_id) {
        let stats = gate.stats();
        let limits = gate.config().limits;
        // Running calls, not slots held: a bus reactor holds reserved slots.
        status.in_flight = stats.running;
        status.waiting = stats.waiting;
        status.max_concurrency = limits.max_concurrency;
        status.rejected_calls_total = stats.rejected_calls;
        status.rejected_concurrency_total = stats.rejected_concurrency;
        // What the gate counted beyond the last flush is in no sample yet; a
        // service the evaluator has not seen contributes nothing.
        let (unflushed_calls, unflushed_concurrency) =
            flushed.get(&assignment.target_id).map_or((0, 0), |f| {
                (
                    stats.rejected_calls.saturating_sub(f.calls),
                    stats.rejected_concurrency.saturating_sub(f.concurrency),
                )
            });
        status.rejected_calls_today += unflushed_calls;
        status.rejected_concurrency_today += unflushed_concurrency;
        status.call_window = limits.call_limit.zip(stats.window_resets_at_ms).map(
            |((limit, window_secs), resets_at_ms)| CallWindowStatus {
                used: stats.window_calls,
                ran: stats.window_ran,
                limit,
                window_secs,
                resets_at_ms,
            },
        );
    }
    status
}

/// The status of one flow of `org_id`. `NotFound` when the flow has no
/// assignment in that organization.
pub fn service_status(
    pool: &DbPool,
    registry: &SlaRegistry,
    views: &WindowViews,
    org_id: &str,
    flow_id: &str,
    day_start_ms: i64,
) -> Result<ServiceStatus> {
    let assignment = repository::get_assignment(pool, org_id, SLA_TARGET_FLOW, flow_id)?.ok_or(
        SlaError::NotFound {
            entity: "assignment",
            id: flow_id.to_string(),
        },
    )?;
    let _ = registry.refresh_if_stale(pool);
    let open = breach::open_breaches_of_org(pool, org_id, Some(flow_id))?;
    let (sampled, flushed) = today_so_far(pool, views, OrgScope::Only(org_id), day_start_ms)?;
    Ok(status_of(
        registry,
        views,
        &assignment,
        open,
        &sampled,
        &flushed,
    ))
}

/// The status of every governed flow of `org_id`, ordered by flow id.
pub fn list_service_statuses(
    pool: &DbPool,
    registry: &SlaRegistry,
    views: &WindowViews,
    org_id: &str,
    day_start_ms: i64,
) -> Result<Vec<ServiceStatus>> {
    let _ = registry.refresh_if_stale(pool);
    let mut open: HashMap<String, Vec<Breach>> = HashMap::new();
    for breach in breach::open_breaches_of_org(pool, org_id, None)? {
        open.entry(breach.target_id.clone())
            .or_default()
            .push(breach);
    }
    let (sampled, flushed) = today_so_far(pool, views, OrgScope::Only(org_id), day_start_ms)?;
    Ok(repository::list_assignments(pool, org_id)?
        .iter()
        .filter(|a| a.target_kind == SLA_TARGET_FLOW)
        .map(|a| {
            let breaches = open.remove(&a.target_id).unwrap_or_default();
            status_of(registry, views, a, breaches, &sampled, &flushed)
        })
        .collect())
}

/// The status of every flow governed by an organization other than `org_id`:
/// what the callers of such a flow actually meet, for the caller to explain a
/// limit it did not set.
pub fn list_foreign_service_statuses(
    pool: &DbPool,
    registry: &SlaRegistry,
    views: &WindowViews,
    org_id: &str,
    day_start_ms: i64,
) -> Result<Vec<ServiceStatus>> {
    let _ = registry.refresh_if_stale(pool);
    let mut open: HashMap<String, Vec<Breach>> = HashMap::new();
    for breach in breach::open_breaches_outside_org(pool, org_id)? {
        open.entry(breach.target_id.clone())
            .or_default()
            .push(breach);
    }
    let (sampled, flushed) = today_so_far(pool, views, OrgScope::AllBut(org_id), day_start_ms)?;
    Ok(repository::list_assignments_outside(pool, org_id)?
        .iter()
        .filter(|a| a.target_kind == SLA_TARGET_FLOW)
        .map(|a| {
            let breaches = open.remove(&a.target_id).unwrap_or_default();
            status_of(registry, views, a, breaches, &sampled, &flushed)
        })
        .collect())
}

/// Start of the local day containing `now_ms`, in epoch milliseconds, in the
/// zone `tz`. When midnight does not exist (a DST jump over it) the first
/// existing local time of that day is used.
pub fn day_start_ms<Tz: chrono::TimeZone>(tz: &Tz, now_ms: i64) -> i64 {
    use chrono::Timelike;
    let now = tz
        .timestamp_millis_opt(now_ms)
        .earliest()
        .unwrap_or_else(|| tz.timestamp_millis_opt(0).unwrap());
    let date = now.date_naive();
    (0..24)
        .find_map(|hour| {
            let local = date.and_hms_opt(hour, 0, 0)?;
            tz.from_local_datetime(&local).earliest()
        })
        .map_or(
            now_ms - i64::from(now.num_seconds_from_midnight()) * 1000,
            |t| t.timestamp_millis(),
        )
}

/// [`day_start_ms`] in the node's local zone, for the moment of the call.
pub fn local_day_start_ms() -> i64 {
    day_start_ms(&chrono::Local, chrono::Utc::now().timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::day_start_ms;
    use chrono::FixedOffset;

    /// 2023-11-14T22:14:00Z.
    const NOW: i64 = 1_700_000_040_000;

    #[test]
    fn the_day_starts_at_local_midnight_and_rolls_over_at_it() {
        let plus_two = FixedOffset::east_opt(2 * 3600).unwrap();
        // 00:14 on the 15th at +02:00: midnight was 14 minutes ago.
        let midnight = NOW - 14 * 60_000;
        assert_eq!(day_start_ms(&plus_two, NOW), midnight);
        assert_eq!(day_start_ms(&plus_two, midnight), midnight);
        // One millisecond earlier is still the previous day.
        assert_eq!(day_start_ms(&plus_two, midnight - 1), midnight - 86_400_000);
        // The same instant is still the 14th at -05:00.
        let minus_five = FixedOffset::west_opt(5 * 3600).unwrap();
        assert_eq!(
            day_start_ms(&minus_five, NOW),
            NOW - (17 * 3600 + 14 * 60) * 1000
        );
    }
}
