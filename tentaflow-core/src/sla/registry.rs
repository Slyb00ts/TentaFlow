//! The node-global map from a service to its gate. It is rebuilt from the
//! stored assignments when the SLA generation moves, and a rebuild adjusts the
//! gates that already exist instead of replacing them, so calls in flight keep
//! their slots, the counters keep counting and the limit never doubles for the
//! length of a reload. A gate whose service lost its policy is not dropped while
//! calls are still in flight: it drains, and a service that is assigned again in
//! the meantime gets the same gate back, so unassign-then-reassign cannot admit
//! a second full set of calls next to the first.
//!
//! Failure policy: when the stored policies cannot be loaded, the gates keep the
//! last configuration that did load (fail open to the last known config). On a
//! cold start there is none, so every service is ungoverned until the first load
//! succeeds (fail open to no limit).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use parking_lot::Mutex;
use tracing::{error, info};

use super::gate::{system_clock, AdmitOrigin, Clock, GateConfig, GateLimits, ServiceGate};
use super::gate::{SlaPermit, SlaRefusal};
use super::policy::SLA_TARGET_FLOW;
use super::repository::{self, Result};
use crate::db::DbPool;

/// After a failed load, the next attempt waits at least this long.
const RETRY_AFTER_FAILURE_MS: u64 = 1_000;

pub struct SlaRegistry {
    gates: DashMap<String, Arc<ServiceGate>>,
    /// Gates of services that lost their policy while calls were in flight.
    draining: DashMap<String, Arc<ServiceGate>>,
    /// When the last load failed, in clock milliseconds; `0` when it did not.
    last_failure_ms: AtomicU64,
    failure_logged: AtomicBool,
    /// The generation the gates were built from; `0` means never loaded.
    loaded_generation: AtomicU64,
    reload_lock: Mutex<()>,
    clock: Clock,
}

impl Default for SlaRegistry {
    fn default() -> Self {
        Self::with_clock(system_clock())
    }
}

/// The registry of this node.
pub fn global() -> &'static SlaRegistry {
    static REGISTRY: OnceLock<SlaRegistry> = OnceLock::new();
    REGISTRY.get_or_init(SlaRegistry::default)
}

impl SlaRegistry {
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            gates: DashMap::new(),
            draining: DashMap::new(),
            last_failure_ms: AtomicU64::new(0),
            failure_logged: AtomicBool::new(false),
            loaded_generation: AtomicU64::new(0),
            reload_lock: Mutex::new(()),
            clock,
        }
    }

    pub fn gate(&self, flow_id: &str) -> Option<Arc<ServiceGate>> {
        self.gates.get(flow_id).map(|entry| entry.value().clone())
    }

    pub fn len(&self) -> usize {
        self.gates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.gates.is_empty()
    }

    /// Every gate with its flow id, for the sampler and the evaluator.
    pub fn gates(&self) -> Vec<(String, Arc<ServiceGate>)> {
        self.gates
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    /// Makes the set of gates equal to `configs`: a new service gets a gate (or
    /// the draining one it had), an existing one is reconfigured in place, and a
    /// service that is no longer governed loses its gate once nothing is in
    /// flight on it.
    pub fn replace(&self, configs: Vec<(String, GateConfig)>) {
        let mut keep = HashSet::with_capacity(configs.len());
        for (flow_id, config) in configs {
            match self.gates.entry(flow_id.clone()) {
                dashmap::mapref::entry::Entry::Occupied(entry) => {
                    entry.get().reconfigure(config);
                }
                dashmap::mapref::entry::Entry::Vacant(entry) => {
                    let gate = match self.draining.remove(&flow_id) {
                        Some((_, gate)) => {
                            gate.reconfigure(config);
                            gate
                        }
                        None => ServiceGate::new(flow_id.clone(), config, self.clock.clone()),
                    };
                    entry.insert(gate);
                }
            }
            keep.insert(flow_id);
        }
        let removed: Vec<String> = self
            .gates
            .iter()
            .filter(|entry| !keep.contains(entry.key()))
            .map(|entry| entry.key().clone())
            .collect();
        for flow_id in removed {
            if let Some((_, gate)) = self.gates.remove(&flow_id) {
                if gate.is_busy() {
                    self.draining.insert(flow_id, gate);
                }
            }
        }
        self.draining.retain(|_, gate| gate.is_busy());
    }

    /// Gates that no longer govern a service but still have calls in flight.
    pub fn draining_len(&self) -> usize {
        self.draining.retain(|_, gate| gate.is_busy());
        self.draining.len()
    }

    /// Reloads from the database when a policy or an assignment changed since
    /// the last load. The generation is read BEFORE the rows, so a change that
    /// lands during the load leaves the registry stale and the next call loads
    /// again. Returns whether a reload happened.
    ///
    /// It never waits: if another thread is already reloading, the caller keeps
    /// serving the current gates. A failed load keeps the last good gates (see
    /// the module docs), is logged once until a load succeeds again, and is not
    /// retried more often than once a second; those attempts return `Ok(false)`.
    /// The load itself is a short blocking read; an async caller on a hot path
    /// can run this on `spawn_blocking`.
    pub fn refresh_if_stale(&self, pool: &DbPool) -> Result<bool> {
        if self.loaded_generation.load(Ordering::Acquire) == super::generation() {
            return Ok(false);
        }
        let failed_at = self.last_failure_ms.load(Ordering::Acquire);
        if failed_at != 0 && (self.clock)().saturating_sub(failed_at) < RETRY_AFTER_FAILURE_MS {
            return Ok(false);
        }
        let Some(_reload) = self.reload_lock.try_lock() else {
            return Ok(false);
        };
        let generation = super::generation();
        if self.loaded_generation.load(Ordering::Acquire) == generation {
            return Ok(false);
        }
        let active = match repository::load_active(pool) {
            Ok(active) => active,
            Err(e) => {
                self.last_failure_ms
                    .store((self.clock)().max(1), Ordering::Release);
                if !self.failure_logged.swap(true, Ordering::AcqRel) {
                    error!(
                        error = %e,
                        "SLA policies could not be loaded; keeping the last known configuration"
                    );
                }
                return Err(e);
            }
        };
        self.last_failure_ms.store(0, Ordering::Release);
        if self.failure_logged.swap(false, Ordering::AcqRel) {
            info!("SLA policies loaded again");
        }
        let configs = active
            .into_iter()
            .filter(|active| active.target_kind == SLA_TARGET_FLOW)
            .map(|active| {
                (
                    active.target_id,
                    GateConfig {
                        policy_id: active.policy_id,
                        org_id: active.org_id,
                        limits: GateLimits::from_spec(&active.spec),
                    },
                )
            })
            .collect();
        self.replace(configs);
        self.loaded_generation.store(generation, Ordering::Release);
        Ok(true)
    }

    /// Admits one call of `flow_id`. A flow without a policy and any call
    /// below the top level (`depth > 0`: a subflow, or a re-entry through an
    /// alias) get an untracked permit: a nested call that took a second slot of
    /// the same gate could wait for the slot its own caller holds.
    pub async fn admit(
        &self,
        flow_id: &str,
        origin: AdmitOrigin,
        depth: u32,
    ) -> std::result::Result<SlaPermit, SlaRefusal> {
        if depth > 0 {
            return Ok(SlaPermit::untracked());
        }
        match self.gate(flow_id) {
            Some(gate) => gate.admit(origin).await,
            None => Ok(SlaPermit::untracked()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sla::gate::{Outcome, RefusalReason};
    use crate::sla::policy::SlaPolicySpec;
    use std::time::Duration;

    fn config(max_concurrency: u32, queue_timeout_ms: u64) -> GateConfig {
        GateConfig {
            policy_id: "p1".into(),
            org_id: "o1".into(),
            limits: GateLimits {
                max_concurrency: Some(max_concurrency),
                max_queue: 0,
                queue_timeout: Duration::from_millis(queue_timeout_ms),
                call_limit: None,
                timeout: None,
            },
        }
    }

    #[tokio::test]
    async fn an_ungoverned_flow_and_a_nested_call_take_no_slot() {
        let registry = SlaRegistry::default();
        registry.replace(vec![("flow-1".into(), config(1, 10))]);

        let permit = registry
            .admit("other", AdmitOrigin::Request, 0)
            .await
            .unwrap();
        assert!(!permit.is_tracked());

        let top = registry
            .admit("flow-1", AdmitOrigin::Request, 0)
            .await
            .unwrap();
        assert!(top.is_tracked());
        let nested = registry
            .admit("flow-1", AdmitOrigin::Request, 1)
            .await
            .expect("a subflow of the same flow must not wait for its caller's slot");
        assert!(!nested.is_tracked());
        let refusal = registry
            .admit("flow-1", AdmitOrigin::Request, 0)
            .await
            .unwrap_err();
        assert_eq!(refusal.reason, RefusalReason::QueueFull);
        assert_eq!(refusal.target_id, "flow-1");
        drop(top);
    }

    #[tokio::test]
    async fn a_reload_keeps_the_gate_its_counters_and_the_permits_in_flight() {
        let registry = SlaRegistry::default();
        registry.replace(vec![("flow-1".into(), config(2, 10))]);
        let before = registry.gate("flow-1").unwrap();
        let a = registry
            .admit("flow-1", AdmitOrigin::Request, 0)
            .await
            .unwrap();
        let b = registry
            .admit("flow-1", AdmitOrigin::Request, 0)
            .await
            .unwrap();

        registry.replace(vec![("flow-1".into(), config(2, 10))]);
        assert!(Arc::ptr_eq(&before, &registry.gate("flow-1").unwrap()));
        assert_eq!(before.stats().in_flight, 2);

        registry.replace(vec![("flow-1".into(), config(3, 10))]);
        assert!(Arc::ptr_eq(&before, &registry.gate("flow-1").unwrap()));
        assert_eq!(
            before.stats().in_flight,
            2,
            "in flight survives a changed limit"
        );
        let c = registry
            .admit("flow-1", AdmitOrigin::Request, 0)
            .await
            .unwrap();
        assert!(
            registry
                .admit("flow-1", AdmitOrigin::Request, 0)
                .await
                .is_err(),
            "three in flight fill the raised limit of 3, nothing extra is admitted"
        );

        registry.replace(Vec::new());
        assert!(registry.gate("flow-1").is_none());
        assert!(registry.is_empty());
        // The permits of a gate that is gone still end cleanly.
        a.finish(Outcome::Ok);
        b.finish(Outcome::Error);
        c.finish(Outcome::Ok);
        let stats = before.stats();
        assert_eq!((stats.in_flight, stats.ok, stats.errors), (0, 2, 1));
    }

    #[tokio::test]
    async fn a_permit_taken_before_a_reload_counts_against_the_new_limit() {
        let registry = SlaRegistry::default();
        registry.replace(vec![("flow-1".into(), config(2, 10))]);
        let a = registry
            .admit("flow-1", AdmitOrigin::Request, 0)
            .await
            .unwrap();
        let b = registry
            .admit("flow-1", AdmitOrigin::Request, 0)
            .await
            .unwrap();
        registry.replace(vec![("flow-1".into(), config(1, 10))]);
        drop(a);
        assert!(
            registry
                .admit("flow-1", AdmitOrigin::Request, 0)
                .await
                .is_err(),
            "b is still in flight under the new limit of 1"
        );
        drop(b);
        drop(
            registry
                .admit("flow-1", AdmitOrigin::Request, 0)
                .await
                .unwrap(),
        );
    }

    #[tokio::test]
    async fn refresh_loads_the_assignments_and_follows_policy_edits() {
        let pool = repository::test_pool();
        let registry = SlaRegistry::default();
        let spec = SlaPolicySpec {
            name: "Registration API".into(),
            max_concurrency: Some(2),
            ..Default::default()
        };
        let policy = repository::create_policy(&pool, "org-a", "admin", &spec).unwrap();
        repository::assign(&pool, "org-a", "admin", "flow", "flow-1", &policy.id).unwrap();

        assert!(registry.refresh_if_stale(&pool).unwrap());
        let gate = registry
            .gate("flow-1")
            .expect("the assigned flow is governed");
        assert_eq!(gate.config().limits.max_concurrency, Some(2));
        assert_eq!(gate.config().policy_id, policy.id);
        assert_eq!(gate.config().org_id, "org-a");

        let held = registry
            .admit("flow-1", AdmitOrigin::Request, 0)
            .await
            .unwrap();
        let edited = SlaPolicySpec {
            max_concurrency: Some(4),
            ..spec
        };
        repository::update_policy(&pool, "org-a", "admin", &policy.id, &edited).unwrap();
        assert!(
            registry.refresh_if_stale(&pool).unwrap(),
            "an edit moves the generation"
        );
        let same = registry.gate("flow-1").unwrap();
        assert!(Arc::ptr_eq(&gate, &same));
        assert_eq!(same.config().limits.max_concurrency, Some(4));
        assert_eq!(same.stats().in_flight, 1);

        repository::unassign(&pool, "org-a", "admin", "flow", "flow-1").unwrap();
        assert!(registry.refresh_if_stale(&pool).unwrap());
        assert!(registry.gate("flow-1").is_none());
        drop(held);
    }

    #[tokio::test]
    async fn unassign_then_reassign_reuses_the_gate_that_is_still_draining() {
        let registry = SlaRegistry::default();
        registry.replace(vec![("flow-1".into(), config(1, 10))]);
        let first = registry.gate("flow-1").unwrap();
        let held = registry
            .admit("flow-1", AdmitOrigin::Request, 0)
            .await
            .unwrap();

        registry.replace(Vec::new());
        assert!(registry.gate("flow-1").is_none());
        assert_eq!(
            registry.draining_len(),
            1,
            "a call is still in flight on it"
        );

        registry.replace(vec![("flow-1".into(), config(1, 10))]);
        assert!(
            Arc::ptr_eq(&first, &registry.gate("flow-1").unwrap()),
            "the draining gate is taken back"
        );
        assert_eq!(registry.draining_len(), 0);
        assert!(
            registry
                .admit("flow-1", AdmitOrigin::Request, 0)
                .await
                .is_err(),
            "the call from before the unassign still holds the one slot"
        );
        drop(held);
        drop(
            registry
                .admit("flow-1", AdmitOrigin::Request, 0)
                .await
                .unwrap(),
        );
    }

    #[tokio::test]
    async fn an_idle_gate_is_dropped_and_a_drained_one_is_forgotten() {
        let registry = SlaRegistry::default();
        registry.replace(vec![
            ("idle".into(), config(1, 10)),
            ("busy".into(), config(1, 10)),
        ]);
        let held = registry
            .admit("busy", AdmitOrigin::Request, 0)
            .await
            .unwrap();
        registry.replace(Vec::new());
        assert_eq!(registry.draining_len(), 1, "only the busy gate drains");
        drop(held);
        assert_eq!(registry.draining_len(), 0);
    }

    #[test]
    fn a_failed_load_keeps_the_last_gates_and_retries_at_most_once_a_second() {
        use std::sync::atomic::AtomicU64;
        let now = Arc::new(AtomicU64::new(10_000));
        let handle = now.clone();
        let registry = SlaRegistry::with_clock(Arc::new(move || handle.load(Ordering::SeqCst)));
        registry.replace(vec![("flow-1".into(), config(2, 10))]);

        // A database whose SLA tables are gone: the load fails.
        let pool = repository::test_pool();
        pool.write()
            .unwrap()
            .execute_batch("PRAGMA foreign_keys = OFF; DROP TABLE service_sla_assignments;")
            .unwrap();
        crate::sla::bump_generation();

        assert!(registry.refresh_if_stale(&pool).is_err());
        assert!(
            registry.gate("flow-1").is_some(),
            "the last good gates stay"
        );
        assert!(
            !registry.refresh_if_stale(&pool).unwrap(),
            "no retry within a second"
        );
        now.store(11_000, Ordering::SeqCst);
        assert!(
            registry.refresh_if_stale(&pool).is_err(),
            "retried after a second"
        );
        assert!(registry.gate("flow-1").is_some());
    }
}
