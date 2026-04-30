// === File: liveness.rs — periodic liveness ticks driving registry transitions ===

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

use crate::mesh::peer_registry::{ConnectionState, ConnectionStateTag, PeerRegistry, StateTrigger};

/// How often we walk the registry to check heartbeat ages.
const TICK_INTERVAL: Duration = Duration::from_secs(5);
/// Connected peer with no app heartbeat for this long is degraded.
const DEGRADE_AFTER: Duration = Duration::from_secs(15);
/// Degraded peer untouched for this long should be flipped to Reconnecting.
const RECONNECT_AFTER: Duration = Duration::from_secs(45);
/// Reconnecting peer stuck for this long is given up to Offline.
const OFFLINE_AFTER: Duration = Duration::from_secs(5 * 60);

/// Periodic scanner that translates "no heartbeat for N seconds" into
/// LivenessTick triggers. The state machine in peer_registry::state owns
/// the actual Connected→Degraded→Reconnecting→Offline transitions.
pub struct LivenessTask {
    registry: Arc<PeerRegistry>,
    tick_interval: Duration,
}

impl LivenessTask {
    pub fn new(registry: Arc<PeerRegistry>) -> Arc<Self> {
        Arc::new(Self {
            registry,
            tick_interval: TICK_INTERVAL,
        })
    }

    pub fn spawn(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    async fn run(self: Arc<Self>) {
        let mut interval = tokio::time::interval(self.tick_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            self.scan_once();
        }
    }

    fn scan_once(&self) {
        let now = Instant::now();
        for shard_idx in 0..self.registry.shard_count() {
            for entry_arc in self.registry.shard_iter(shard_idx) {
                let (id, conn_tag, last_hb, since) = {
                    let e = entry_arc.read();
                    (
                        e.node_id,
                        ConnectionStateTag::from(&e.conn),
                        e.last_app_heartbeat,
                        conn_since(&e.conn),
                    )
                };

                let hb_age = last_hb.map(|t| now.saturating_duration_since(t));

                let needs_tick = match conn_tag {
                    ConnectionStateTag::Connected => match hb_age {
                        Some(d) => d >= DEGRADE_AFTER,
                        // No heartbeat has been recorded yet — only flip
                        // to Degraded once the connection itself has been
                        // up long enough that we should have seen one.
                        None => since
                            .map(|s| now.saturating_duration_since(s) >= DEGRADE_AFTER)
                            .unwrap_or(false),
                    },
                    ConnectionStateTag::Degraded => match hb_age {
                        Some(d) => d >= RECONNECT_AFTER,
                        None => since
                            .map(|s| now.saturating_duration_since(s) >= RECONNECT_AFTER)
                            .unwrap_or(true),
                    },
                    ConnectionStateTag::Reconnecting => since
                        .map(|s| now.saturating_duration_since(s) >= OFFLINE_AFTER)
                        .unwrap_or(false),
                    _ => false,
                };

                if needs_tick {
                    self.registry
                        .transition_state(&id, StateTrigger::LivenessTick { now });
                }
            }
        }
    }
}

fn conn_since(conn: &ConnectionState) -> Option<Instant> {
    match conn {
        ConnectionState::Connecting { since, .. }
        | ConnectionState::Connected { since, .. }
        | ConnectionState::Degraded { since, .. }
        | ConnectionState::Reconnecting { since, .. }
        | ConnectionState::Offline { since } => Some(*since),
        ConnectionState::Disconnected => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::peer_registry::{
        ActivePath, DialPath, NodeId, StateTrigger as Trigger, TransportHints,
    };
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

    fn nid(b: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = b;
        id
    }

    fn dummy_path() -> ActivePath {
        ActivePath::Direct {
            addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 9000)),
        }
    }

    fn force_connected(registry: &PeerRegistry, id: NodeId) {
        registry.transition_state(
            &id,
            Trigger::DialStarted {
                via: DialPath::Direct,
            },
        );
        registry.transition_state(
            &id,
            Trigger::DialOk {
                conn_id: 1,
                path: dummy_path(),
            },
        );
    }

    /// Connected with last heartbeat 20s ago → LivenessTick promotes to Degraded.
    #[test]
    fn liveness_tick_promotes_connected_to_degraded_after_15s() {
        let registry = PeerRegistry::new(64);
        let id = nid(1);
        registry.upsert_discovered(id, TransportHints::default());
        force_connected(&registry, id);

        // Backdate last_app_heartbeat to 20s ago.
        for shard_idx in 0..registry.shard_count() {
            for arc in registry.shard_iter(shard_idx) {
                let mut g = arc.write();
                if g.node_id == id {
                    g.last_app_heartbeat = Some(Instant::now() - Duration::from_secs(20));
                }
            }
        }

        let task = LivenessTask::new(registry.clone());
        task.scan_once();

        let summary = registry.snapshot_summary();
        let s = summary
            .iter()
            .find(|s| s.node_id == id)
            .expect("entry exists");
        assert_eq!(s.conn_tag, ConnectionStateTag::Degraded);
    }

    /// Degraded with last heartbeat 50s ago → LivenessTick flips to Reconnecting.
    #[test]
    fn liveness_tick_promotes_degraded_to_reconnecting_after_45s_no_hb() {
        let registry = PeerRegistry::new(64);
        let id = nid(2);
        registry.upsert_discovered(id, TransportHints::default());
        force_connected(&registry, id);

        // Backdate to 50s ago, then run once → should land in Degraded.
        for shard_idx in 0..registry.shard_count() {
            for arc in registry.shard_iter(shard_idx) {
                let mut g = arc.write();
                if g.node_id == id {
                    g.last_app_heartbeat = Some(Instant::now() - Duration::from_secs(50));
                }
            }
        }
        let task = LivenessTask::new(registry.clone());
        task.scan_once();
        // First tick: Connected → Degraded.
        // Second tick (with the same backdate): Degraded → Reconnecting.
        task.scan_once();

        let summary = registry.snapshot_summary();
        let s = summary
            .iter()
            .find(|s| s.node_id == id)
            .expect("entry exists");
        assert_eq!(s.conn_tag, ConnectionStateTag::Reconnecting);
    }

    /// Connected with a fresh heartbeat is left alone.
    #[test]
    fn liveness_tick_keeps_fresh_connected() {
        let registry = PeerRegistry::new(64);
        let id = nid(3);
        registry.upsert_discovered(id, TransportHints::default());
        force_connected(&registry, id);

        // Set heartbeat to "now" so the scan considers the peer alive.
        registry.record_heartbeat(&id, Instant::now());
        let task = LivenessTask::new(registry.clone());
        task.scan_once();

        let summary = registry.snapshot_summary();
        let s = summary
            .iter()
            .find(|s| s.node_id == id)
            .expect("entry exists");
        assert_eq!(s.conn_tag, ConnectionStateTag::Connected);
    }
}
