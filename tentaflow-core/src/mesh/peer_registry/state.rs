// === File: peer_registry/state.rs — connection state machine, pure transition fn ===

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::mesh::peer_registry::entry::{ArcStr, RetryState};

/// Liveness threshold: connected peer with no app heartbeat for this long is degraded.
pub const LIVENESS_DEGRADE_AFTER: Duration = Duration::from_secs(15);
/// Degraded peer with no heartbeat for this long is considered unreachable -> reconnect.
pub const LIVENESS_RECONNECT_AFTER: Duration = Duration::from_secs(45);
/// Reconnect-stuck timeout: after 5 minutes in Reconnecting we give up to Offline.
pub const RECONNECT_GIVEUP_AFTER: Duration = Duration::from_secs(5 * 60);
/// Reconnect attempt cap: more than 12 attempts -> give up to Offline.
pub const RECONNECT_GIVEUP_ATTEMPTS: u32 = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialPath {
    Direct,
    Relay,
    DhtLookup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivePath {
    Direct { addr: SocketAddr },
    Relay { url: ArcStr },
}

#[derive(Debug, Clone)]
pub enum ConnectionState {
    Disconnected,
    Connecting {
        since: Instant,
        via: DialPath,
    },
    Connected {
        since: Instant,
        path: ActivePath,
        conn_id: u64,
    },
    Degraded {
        since: Instant,
        path: ActivePath,
        conn_id: u64,
        missed_heartbeats: u8,
    },
    Reconnecting {
        since: Instant,
        attempt: u32,
        backoff_until: Instant,
        last_err: ArcStr,
    },
    Offline {
        since: Instant,
    },
}

#[derive(Debug, Clone)]
pub enum StateTrigger {
    Discovered,
    DialStarted { via: DialPath },
    DialOk { conn_id: u64, path: ActivePath },
    DialFail { err: ArcStr },
    TransportClosed { conn_id: u64 },
    Heartbeat { at: Instant },
    LivenessTick { now: Instant },
    RetryTimer { now: Instant },
    HintFailedTwice,
    Forget,
    TrustGranted,
}

#[derive(Debug, Clone)]
pub enum TransitionSideEffect {
    ScheduleDial { at: Instant },
    ResetRetry,
    BumpRetry { err: ArcStr },
    InvalidateHint,
    EmitOnline,
    EmitOffline,
    EmitForget,
}

#[derive(Debug, Clone, Default)]
pub struct TransitionResult {
    /// New state, or None if no state change.
    pub new_state: Option<ConnectionState>,
    pub side_effect: Option<TransitionSideEffect>,
}

impl TransitionResult {
    fn no_change() -> Self {
        Self { new_state: None, side_effect: None }
    }
    fn state(s: ConnectionState) -> Self {
        Self { new_state: Some(s), side_effect: None }
    }
    fn state_with(s: ConnectionState, eff: TransitionSideEffect) -> Self {
        Self { new_state: Some(s), side_effect: Some(eff) }
    }
    fn effect_only(eff: TransitionSideEffect) -> Self {
        Self { new_state: None, side_effect: Some(eff) }
    }
}

/// Exponential backoff capped at 5 minutes. Caller may add jitter.
/// bk(1)=1s, bk(2)=2s, bk(3)=4s, bk(11+)=300s.
pub fn backoff(attempt: u32) -> Duration {
    let base_ms: u64 = 1000;
    let cap_ms: u64 = 5 * 60 * 1000;
    let shift = attempt.saturating_sub(1).min(20);
    let exp = base_ms.saturating_mul(1u64 << shift);
    Duration::from_millis(exp.min(cap_ms))
}

/// Pure state transition. Does NOT touch heartbeat fields, retry counters or
/// I/O — those live on PeerEntry / RetryState. The registry layer applies the
/// returned new_state and side effect.
pub fn transition(
    current: &ConnectionState,
    retry: &RetryState,
    trigger: &StateTrigger,
    now: Instant,
) -> TransitionResult {
    match (current, trigger) {
        // ---- Discovered ---------------------------------------------------
        (ConnectionState::Offline { .. }, StateTrigger::Discovered) => {
            TransitionResult::state_with(
                ConnectionState::Disconnected,
                TransitionSideEffect::ScheduleDial { at: now },
            )
        }
        (ConnectionState::Disconnected, StateTrigger::Discovered) => {
            TransitionResult::effect_only(TransitionSideEffect::ScheduleDial { at: now })
        }
        (_, StateTrigger::Discovered) => TransitionResult::no_change(),

        // ---- DialStarted --------------------------------------------------
        (
            ConnectionState::Disconnected | ConnectionState::Offline { .. },
            StateTrigger::DialStarted { via },
        ) => TransitionResult::state(ConnectionState::Connecting {
            since: now,
            via: via.clone(),
        }),
        (_, StateTrigger::DialStarted { .. }) => TransitionResult::no_change(),

        // ---- DialOk -------------------------------------------------------
        (
            ConnectionState::Connecting { .. } | ConnectionState::Reconnecting { .. },
            StateTrigger::DialOk { conn_id, path },
        ) => TransitionResult::state_with(
            ConnectionState::Connected {
                since: now,
                path: path.clone(),
                conn_id: *conn_id,
            },
            TransitionSideEffect::EmitOnline,
        ),
        (_, StateTrigger::DialOk { .. }) => TransitionResult::no_change(),

        // ---- DialFail -----------------------------------------------------
        (ConnectionState::Connecting { .. }, StateTrigger::DialFail { err }) => {
            let attempt = 1u32;
            let backoff_until = now + backoff(attempt);
            TransitionResult {
                new_state: Some(ConnectionState::Reconnecting {
                    since: now,
                    attempt,
                    backoff_until,
                    last_err: err.clone(),
                }),
                side_effect: Some(TransitionSideEffect::BumpRetry { err: err.clone() }),
            }
        }
        (
            ConnectionState::Reconnecting { since, attempt, .. },
            StateTrigger::DialFail { err },
        ) => {
            let next_attempt = attempt.saturating_add(1);
            // Give up if we exceed the attempt cap.
            if next_attempt > RECONNECT_GIVEUP_ATTEMPTS {
                return TransitionResult::state_with(
                    ConnectionState::Offline { since: now },
                    TransitionSideEffect::EmitOffline,
                );
            }
            let backoff_until = now + backoff(next_attempt);
            TransitionResult {
                new_state: Some(ConnectionState::Reconnecting {
                    since: *since,
                    attempt: next_attempt,
                    backoff_until,
                    last_err: err.clone(),
                }),
                side_effect: Some(TransitionSideEffect::BumpRetry { err: err.clone() }),
            }
        }
        (_, StateTrigger::DialFail { .. }) => TransitionResult::no_change(),

        // ---- TransportClosed ---------------------------------------------
        (
            ConnectionState::Connected { conn_id, .. }
            | ConnectionState::Degraded { conn_id, .. },
            StateTrigger::TransportClosed { conn_id: closed_id },
        ) => {
            if conn_id != closed_id {
                // Out-of-order event for a stale conn_id — ignore.
                TransitionResult::no_change()
            } else {
                let attempt = 1u32;
                let err: ArcStr = std::sync::Arc::<str>::from("transport_closed");
                let backoff_until = now + backoff(attempt);
                TransitionResult {
                    new_state: Some(ConnectionState::Reconnecting {
                        since: now,
                        attempt,
                        backoff_until,
                        last_err: err.clone(),
                    }),
                    side_effect: Some(TransitionSideEffect::ScheduleDial { at: now }),
                }
            }
        }
        (_, StateTrigger::TransportClosed { .. }) => TransitionResult::no_change(),

        // ---- Heartbeat ----------------------------------------------------
        (ConnectionState::Connected { .. }, StateTrigger::Heartbeat { .. }) => {
            // PeerEntry updates last_app_heartbeat; state stays Connected.
            TransitionResult::no_change()
        }
        (
            ConnectionState::Degraded { path, conn_id, .. },
            StateTrigger::Heartbeat { .. },
        ) => TransitionResult::state(ConnectionState::Connected {
            since: now,
            path: path.clone(),
            conn_id: *conn_id,
        }),
        (_, StateTrigger::Heartbeat { .. }) => TransitionResult::no_change(),

        // ---- LivenessTick -------------------------------------------------
        (
            ConnectionState::Connected { path, conn_id, .. },
            StateTrigger::LivenessTick { now: tick_now },
        ) => {
            // PeerEntry stores last_app_heartbeat; the registry translates
            // "no heartbeat for 15s" into a LivenessTick that arrives here.
            // The threshold check is done by the caller (registry) — by the
            // time we are called for Connected with LivenessTick we treat it
            // as "missed >=15s, transition to Degraded".
            TransitionResult::state(ConnectionState::Degraded {
                since: *tick_now,
                path: path.clone(),
                conn_id: *conn_id,
                missed_heartbeats: 1,
            })
        }
        (
            ConnectionState::Degraded { .. },
            StateTrigger::LivenessTick { now: tick_now },
        ) => {
            let attempt = 1u32;
            let err: ArcStr = std::sync::Arc::<str>::from("liveness_timeout");
            let backoff_until = *tick_now + backoff(attempt);
            TransitionResult {
                new_state: Some(ConnectionState::Reconnecting {
                    since: *tick_now,
                    attempt,
                    backoff_until,
                    last_err: err,
                }),
                side_effect: Some(TransitionSideEffect::ScheduleDial { at: *tick_now }),
            }
        }
        (
            ConnectionState::Reconnecting { since, .. },
            StateTrigger::LivenessTick { now: tick_now },
        ) => {
            let stuck_too_long =
                tick_now.saturating_duration_since(*since) >= RECONNECT_GIVEUP_AFTER;
            let too_many_attempts = retry.attempts > RECONNECT_GIVEUP_ATTEMPTS;
            if stuck_too_long || too_many_attempts {
                TransitionResult::state_with(
                    ConnectionState::Offline { since: *tick_now },
                    TransitionSideEffect::EmitOffline,
                )
            } else {
                TransitionResult::no_change()
            }
        }
        (_, StateTrigger::LivenessTick { .. }) => TransitionResult::no_change(),

        // ---- RetryTimer ---------------------------------------------------
        (
            ConnectionState::Reconnecting {
                since,
                attempt,
                backoff_until,
                last_err,
            },
            StateTrigger::RetryTimer { now: tick_now },
        ) => {
            if *tick_now < *backoff_until {
                TransitionResult::no_change()
            } else {
                // The path is chosen by ReconnectManager (PR4). We move to
                // Connecting with a placeholder — the registry/manager will
                // emit DialStarted with the real DialPath shortly.
                let _ = (since, attempt, last_err);
                TransitionResult::state(ConnectionState::Connecting {
                    since: *tick_now,
                    via: DialPath::Direct,
                })
            }
        }
        (_, StateTrigger::RetryTimer { .. }) => TransitionResult::no_change(),

        // ---- HintFailedTwice ---------------------------------------------
        (ConnectionState::Reconnecting { .. }, StateTrigger::HintFailedTwice) => {
            TransitionResult::effect_only(TransitionSideEffect::InvalidateHint)
        }
        (_, StateTrigger::HintFailedTwice) => TransitionResult::no_change(),

        // ---- Forget -------------------------------------------------------
        (_, StateTrigger::Forget) => {
            TransitionResult::effect_only(TransitionSideEffect::EmitForget)
        }

        // ---- TrustGranted -------------------------------------------------
        (_, StateTrigger::TrustGranted) => TransitionResult::no_change(),
    }
}
