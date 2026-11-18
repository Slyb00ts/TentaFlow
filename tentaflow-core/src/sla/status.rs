//! The current state of a governed service, as the protocol layer shows it:
//! what is in flight against the concurrency limit, how much of the call
//! window is used and when it resets, what the last evaluation window saw
//! (p95, error rate, rejections) and which breaches are open.
//!
//! Reads are pinned to one organization: an assignment of another one reads as
//! missing, so the caller answers `not_found`.

use std::collections::HashMap;

use super::breach::{self, Breach, BreachKind};
use super::evaluator::{WindowSummary, WindowViews};
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

fn status_of(
    registry: &SlaRegistry,
    views: &WindowViews,
    assignment: &SlaAssignment,
    open_breaches: Vec<Breach>,
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
        open_breaches,
    };
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
) -> Result<ServiceStatus> {
    let assignment = repository::get_assignment(pool, org_id, SLA_TARGET_FLOW, flow_id)?.ok_or(
        SlaError::NotFound {
            entity: "assignment",
            id: flow_id.to_string(),
        },
    )?;
    let _ = registry.refresh_if_stale(pool);
    let open = breach::open_breaches_of_org(pool, org_id, Some(flow_id))?;
    Ok(status_of(registry, views, &assignment, open))
}

/// The status of every governed flow of `org_id`, ordered by flow id.
pub fn list_service_statuses(
    pool: &DbPool,
    registry: &SlaRegistry,
    views: &WindowViews,
    org_id: &str,
) -> Result<Vec<ServiceStatus>> {
    let _ = registry.refresh_if_stale(pool);
    let mut open: HashMap<String, Vec<Breach>> = HashMap::new();
    for breach in breach::open_breaches_of_org(pool, org_id, None)? {
        open.entry(breach.target_id.clone())
            .or_default()
            .push(breach);
    }
    Ok(repository::list_assignments(pool, org_id)?
        .iter()
        .filter(|a| a.target_kind == SLA_TARGET_FLOW)
        .map(|a| {
            let breaches = open.remove(&a.target_id).unwrap_or_default();
            status_of(registry, views, a, breaches)
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
) -> Result<Vec<ServiceStatus>> {
    let _ = registry.refresh_if_stale(pool);
    let mut open: HashMap<String, Vec<Breach>> = HashMap::new();
    for breach in breach::open_breaches_outside_org(pool, org_id)? {
        open.entry(breach.target_id.clone())
            .or_default()
            .push(breach);
    }
    Ok(repository::list_assignments_outside(pool, org_id)?
        .iter()
        .filter(|a| a.target_kind == SLA_TARGET_FLOW)
        .map(|a| {
            let breaches = open.remove(&a.target_id).unwrap_or_default();
            status_of(registry, views, a, breaches)
        })
        .collect())
}
