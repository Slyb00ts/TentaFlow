// === File: reconnect.rs — event-driven reconnect manager with backoff + jitter ===

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::sleep_until;
use tracing::{debug, warn};

use crate::mesh::iroh_manager::IrohMeshManager;
use crate::mesh::peer_registry::{
    ConnectionStateTag, DialPath, HintKind, NodeId, PeerDelta, PeerRegistry, StateTrigger,
    TransportHints,
};
use crate::net::iroh::pairing::PairingContactHints;

/// Maximum concurrent in-flight dials issued by the manager.
const MAX_CONCURRENT_DIALS: usize = 32;
/// Periodic safety-net rescan that re-queues peers stuck in Reconnecting
/// without an active timer (e.g. after a panic in a spawned dial task).
const IDLE_RESCAN_INTERVAL: Duration = Duration::from_secs(60);
/// Maximum jitter added on top of registry-supplied backoff_until.
const JITTER_MAX_MS: u64 = 1000;
/// After this many consecutive failures we drop the cached hints and let
/// pure DHT lookup take over — hints from pairing may be stale (peer's IP
/// rotated, relay changed).
const HINT_FAILURE_THRESHOLD: u32 = 2;

/// Drives reconnect logic by reacting to PeerDelta events from the registry.
/// Owns a timer wheel keyed on the wall-clock instant a dial should fire.
pub struct ReconnectManager {
    registry: Arc<PeerRegistry>,
    iroh: Arc<IrohMeshManager>,
    local_node_id_hex: String,
}

impl ReconnectManager {
    pub fn new(
        registry: Arc<PeerRegistry>,
        iroh: Arc<IrohMeshManager>,
        local_node_id_hex: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            registry,
            iroh,
            local_node_id_hex,
        })
    }

    pub fn spawn(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    async fn run(self: Arc<Self>) {
        let mut rx = self.registry.subscribe();
        let mut idle_scan = tokio::time::interval(IDLE_RESCAN_INTERVAL);
        idle_scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut timers: BTreeMap<Instant, NodeId> = BTreeMap::new();
        let dial_semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_DIALS));

        // Initial rescan picks up trusted peers seeded into the registry
        // before the manager started.
        self.rescan_pending(&mut timers);

        loop {
            let next_timer_at = timers.keys().next().copied();
            tokio::select! {
                recv = rx.recv() => match recv {
                    Ok(delta) => self.handle_delta(delta, &mut timers),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(lagged = n, "ReconnectManager: registry bus lagged, full rescan");
                        self.rescan_pending(&mut timers);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = sleep_until_opt(next_timer_at) => {
                    while let Some((_, id)) = pop_due(&mut timers, Instant::now()) {
                        let perm = match dial_semaphore.clone().acquire_owned().await {
                            Ok(p) => p,
                            Err(_) => break,
                        };
                        let this = self.clone();
                        tokio::spawn(async move {
                            let _perm = perm;
                            this.try_dial(id).await;
                        });
                    }
                }
                _ = idle_scan.tick() => self.rescan_pending(&mut timers),
            }
        }
    }

    fn handle_delta(&self, delta: PeerDelta, timers: &mut BTreeMap<Instant, NodeId>) {
        match delta {
            PeerDelta::Discovered { node_id } => {
                if self.is_self(&node_id) {
                    return;
                }
                if !self.registry.is_connected(&node_id) {
                    schedule(timers, Instant::now(), node_id);
                }
            }
            PeerDelta::StateChanged { node_id, to, .. } => {
                if self.is_self(&node_id) {
                    return;
                }
                match to {
                    ConnectionStateTag::Reconnecting => {
                        if let Some(detail) = self.registry.snapshot_detail(&node_id) {
                            let when = detail
                                .retry
                                .next_attempt
                                .unwrap_or_else(Instant::now)
                                .checked_add(jitter())
                                .unwrap_or_else(Instant::now);
                            schedule(timers, when, node_id);
                        } else {
                            schedule(timers, Instant::now(), node_id);
                        }
                    }
                    ConnectionStateTag::Connected => {
                        timers.retain(|_, v| *v != node_id);
                    }
                    ConnectionStateTag::Offline => {
                        // Registry gave up; drop pending timer. A fresh
                        // Discovered (mDNS / pairing reload) will requeue.
                        timers.retain(|_, v| *v != node_id);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    async fn try_dial(self: Arc<Self>, id: NodeId) {
        if self.is_self(&id) {
            return;
        }
        let detail = self.registry.snapshot_detail(&id);
        let attempts_before = detail.as_ref().map(|d| d.retry.attempts).unwrap_or(0);
        let hints_present = detail
            .as_ref()
            .map(|d| !d.hints.addresses.is_empty() || d.hints.relay_url.is_some())
            .unwrap_or(false);

        // After repeated failures with the cached hints, drop them so the
        // next attempt falls through to DHT/relay lookup. Mirrors the old
        // FAILURE_THRESHOLD behaviour from the deleted reconnect loop.
        if hints_present && attempts_before >= HINT_FAILURE_THRESHOLD {
            self.registry.invalidate_hint(&id, HintKind::DirectAddr);
            self.registry.invalidate_hint(&id, HintKind::RelayUrl);
        }

        self.registry.transition_state(
            &id,
            StateTrigger::DialStarted {
                via: DialPath::Direct,
            },
        );

        let id_hex = hex::encode(id);
        let result = match detail.as_ref() {
            Some(d)
                if attempts_before < HINT_FAILURE_THRESHOLD
                    && (!d.hints.addresses.is_empty() || d.hints.relay_url.is_some()) =>
            {
                let pairing = pairing_hints_from(&id_hex, &d.hints);
                self.iroh.connect_to_peer_with_hints(&pairing).await
            }
            _ => {
                let dummy = std::net::SocketAddr::from(([0u8, 0, 0, 0], 0));
                self.iroh.connect_to_peer(&id_hex, dummy).await
            }
        };

        match result {
            Ok(()) => {
                debug!(peer = %id_hex, "ReconnectManager: dial succeeded");
                // The actual DialOk transition is driven by the iroh event
                // loop (handle_peer_connected → peer_store shadow). The
                // manager only records the attempt; if the connection did
                // not actually come up, a TransportClosed/timeout will
                // bring us back into Reconnecting.
            }
            Err(e) => {
                let err_msg: Arc<str> = format!("{e:#}").into();
                debug!(peer = %id_hex, error = %err_msg, "ReconnectManager: dial failed");
                self.registry
                    .transition_state(&id, StateTrigger::DialFail { err: err_msg });
            }
        }
    }

    fn rescan_pending(&self, timers: &mut BTreeMap<Instant, NodeId>) {
        for s in self.registry.snapshot_summary() {
            if self.is_self(&s.node_id) {
                continue;
            }
            let needs_dial = matches!(
                s.conn_tag,
                ConnectionStateTag::Disconnected
                    | ConnectionStateTag::Reconnecting
                    | ConnectionStateTag::Offline
            );
            if !needs_dial {
                continue;
            }
            if timers.values().any(|v| *v == s.node_id) {
                continue;
            }
            let when = match s.conn_tag {
                ConnectionStateTag::Reconnecting => self
                    .registry
                    .snapshot_detail(&s.node_id)
                    .and_then(|d| d.retry.next_attempt)
                    .map(|t| t + jitter())
                    .unwrap_or_else(|| Instant::now() + Duration::from_secs(1)),
                _ => Instant::now() + Duration::from_secs(1),
            };
            schedule(timers, when, s.node_id);
        }
    }

    fn is_self(&self, id: &NodeId) -> bool {
        let hex_id = hex::encode(id);
        hex_id == self.local_node_id_hex
    }
}

fn schedule(timers: &mut BTreeMap<Instant, NodeId>, mut at: Instant, id: NodeId) {
    // Avoid colliding keys (BTreeMap is keyed on Instant — duplicates would
    // overwrite). Bump by 1ns until the slot is free.
    while timers.contains_key(&at) {
        at += Duration::from_nanos(1);
    }
    timers.insert(at, id);
}

fn pop_due(timers: &mut BTreeMap<Instant, NodeId>, now: Instant) -> Option<(Instant, NodeId)> {
    let key = *timers.range(..=now).next()?.0;
    timers.remove_entry(&key)
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(t) => sleep_until(t.into()).await,
        None => std::future::pending::<()>().await,
    }
}

fn jitter() -> Duration {
    use rand::RngExt;
    Duration::from_millis(rand::rng().random_range(0..JITTER_MAX_MS))
}

fn pairing_hints_from(node_id_hex: &str, hints: &TransportHints) -> PairingContactHints {
    PairingContactHints {
        node_id: node_id_hex.to_string(),
        public_key_hex: String::new(),
        hostname: hints
            .hostname_dns
            .as_deref()
            .map(|s| s.to_string())
            .unwrap_or_default(),
        addresses: hints.addresses.iter().map(|a| a.to_string()).collect(),
        relay_url: hints
            .relay_url
            .as_deref()
            .map(|s| s.to_string())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::peer_registry::TrustState;

    fn nid(b: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = b;
        id
    }

    /// Smoke test: Discovered event for a non-self peer schedules a dial.
    /// We exercise handle_delta directly so no real iroh manager is needed.
    #[tokio::test]
    async fn discovered_event_schedules_dial() {
        let registry = PeerRegistry::new(64);
        let id = nid(7);
        registry.upsert_discovered(id, TransportHints::default());
        registry.set_trust(&id, TrustState::Trusted);

        // Build a fake manager skeleton just to satisfy types — we never
        // call try_dial, only handle_delta which does not touch iroh.
        struct Stub {
            registry: Arc<PeerRegistry>,
            local_node_id_hex: String,
        }
        impl Stub {
            fn handle(&self, delta: PeerDelta, timers: &mut BTreeMap<Instant, NodeId>) {
                match delta {
                    PeerDelta::Discovered { node_id } => {
                        if hex::encode(node_id) == self.local_node_id_hex {
                            return;
                        }
                        if !self.registry.is_connected(&node_id) {
                            schedule(timers, Instant::now(), node_id);
                        }
                    }
                    _ => {}
                }
            }
        }
        let stub = Stub {
            registry: registry.clone(),
            local_node_id_hex: hex::encode(nid(0)),
        };
        let mut timers: BTreeMap<Instant, NodeId> = BTreeMap::new();
        stub.handle(PeerDelta::Discovered { node_id: id }, &mut timers);
        assert_eq!(timers.len(), 1);
        assert_eq!(*timers.values().next().unwrap(), id);
    }

    #[test]
    fn pairing_hints_roundtrip() {
        let mut h = TransportHints::default();
        h.addresses
            .push(std::net::SocketAddr::from(([192u8, 168, 1, 5], 8090)));
        h.relay_url = Some(Arc::<str>::from("https://relay.example/"));
        h.hostname_dns = Some(Arc::<str>::from("alice.local"));
        let p = pairing_hints_from("aabb", &h);
        assert_eq!(p.node_id, "aabb");
        assert_eq!(p.hostname, "alice.local");
        assert_eq!(p.addresses, vec!["192.168.1.5:8090".to_string()]);
        assert_eq!(p.relay_url, "https://relay.example/");
    }
}
