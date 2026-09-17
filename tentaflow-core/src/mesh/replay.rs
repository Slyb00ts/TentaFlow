// =============================================================================
// File: mesh/replay.rs
// Purpose: Replay protection for state-changing mesh frames. Wraps the UFP/2
//          `ReplayGuard` with the mesh-specific window, the restart gap rule
//          and a debounce for clock-drift warnings.
// =============================================================================

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tentaflow_sdk_spec::protocol::frame::envelope::{Envelope, NODE_ID_LEN};
use tentaflow_sdk_spec::protocol::frame::error::FrameErrorCode;
use tentaflow_sdk_spec::protocol::frame::replay::ReplayGuard;

/// Cross-node session assertions already require clocks within 120 s, so the
/// replay window uses the same bound. The 30 s frame-spec default would cut
/// off LAN nodes that run without NTP.
const MESH_CLOCK_SKEW_MS: u64 = 120_000;

/// A peer may send 1200 commands a minute, so 2400 guarded frames fit in one
/// window. The cache must hold more than that: an entry evicted while its
/// frame is still inside the window could be replayed.
const GUARDED_FRAMES_PER_SOURCE: usize = 8192;

/// A node with a drifting clock gets every guarded frame refused; one warning
/// per peer per interval keeps that visible without flooding the log.
const REJECTION_WARNING_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayRejection {
    /// The frame's timestamp is more than the window away from the local clock.
    ClockSkew,
    /// The frame was signed before this process started, so the in-memory
    /// cache cannot tell whether it was already applied.
    PredatesStart,
    /// The same message id was already accepted from this source.
    Duplicate,
}

impl ReplayRejection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClockSkew => "clock_skew",
            Self::PredatesStart => "predates_start",
            Self::Duplicate => "duplicate",
        }
    }
}

pub struct MeshReplayGuard {
    guard: ReplayGuard,
    started_at_ms: i64,
    last_warning: DashMap<[u8; NODE_ID_LEN], Instant>,
}

impl MeshReplayGuard {
    pub fn new(started_at_ms: i64) -> Self {
        Self {
            guard: ReplayGuard::new(
                NonZeroUsize::new(GUARDED_FRAMES_PER_SOURCE).expect("capacity is non-zero"),
                MESH_CLOCK_SKEW_MS,
            ),
            started_at_ms,
            last_warning: DashMap::new(),
        }
    }

    /// Admits a guarded frame once. The caller must have verified the
    /// signature, bound `source.id` to the transport peer and checked trust,
    /// so that only trusted peers can allocate a per-source cache.
    pub fn admit(&self, envelope: &Envelope, now_ms: i64) -> Result<(), ReplayRejection> {
        if envelope.created_at_ms < self.started_at_ms {
            return Err(ReplayRejection::PredatesStart);
        }
        self.guard
            .try_observe(envelope, now_ms.max(0) as u64, |_| Ok(()))
            .map_err(|e| match e.code {
                FrameErrorCode::ReplayDetected => ReplayRejection::Duplicate,
                _ => ReplayRejection::ClockSkew,
            })
    }

    /// Returns `true` when a rejection from this source should be reported
    /// now, and `false` while an earlier report is still recent.
    pub fn should_report(&self, source: &[u8; NODE_ID_LEN]) -> bool {
        let now = Instant::now();
        let mut due = true;
        self.last_warning
            .entry(*source)
            .and_modify(|last| {
                if now.duration_since(*last) < REJECTION_WARNING_INTERVAL {
                    due = false;
                } else {
                    *last = now;
                }
            })
            .or_insert(now);
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_sdk_spec::protocol::frame::sign::public_key_bytes;

    const START_MS: i64 = 1_000_000_000;
    const WINDOW_MS: i64 = MESH_CLOCK_SKEW_MS as i64;

    fn envelope_created_at(created_at_ms: i64) -> Envelope {
        let key = crate::crypto::generate_signing_key().expect("signing key");
        let source = public_key_bytes(&key);
        let wire = crate::mesh::ufp2::send::build_signed_envelope_wire(
            &key,
            source,
            [0x22u8; NODE_ID_LEN],
            tentaflow_protocol::mesh::MESH_MSG_COMMAND,
            b"payload".to_vec(),
            5,
        )
        .expect("wire");
        let mut envelope = crate::mesh::ufp2::codec::decode_incoming(&wire)
            .expect("decode")
            .envelope;
        envelope.created_at_ms = created_at_ms;
        envelope
    }

    #[test]
    fn the_same_frame_is_admitted_only_once() {
        let guard = MeshReplayGuard::new(START_MS);
        let envelope = envelope_created_at(START_MS + 5_000);
        assert_eq!(guard.admit(&envelope, START_MS + 5_100), Ok(()));
        assert_eq!(
            guard.admit(&envelope, START_MS + 5_200),
            Err(ReplayRejection::Duplicate)
        );
    }

    #[test]
    fn a_frame_signed_before_the_process_started_is_refused() {
        let guard = MeshReplayGuard::new(START_MS);
        let envelope = envelope_created_at(START_MS - 1);
        assert_eq!(
            guard.admit(&envelope, START_MS + 10),
            Err(ReplayRejection::PredatesStart)
        );
    }

    #[test]
    fn a_frame_outside_the_clock_window_is_refused_in_both_directions() {
        let guard = MeshReplayGuard::new(START_MS);
        let now = START_MS + 1_000_000;
        let stale = envelope_created_at(now - WINDOW_MS - 1);
        let from_the_future = envelope_created_at(now + WINDOW_MS + 1);
        let at_the_edge = envelope_created_at(now - WINDOW_MS);
        assert_eq!(guard.admit(&stale, now), Err(ReplayRejection::ClockSkew));
        assert_eq!(
            guard.admit(&from_the_future, now),
            Err(ReplayRejection::ClockSkew)
        );
        assert_eq!(guard.admit(&at_the_edge, now), Ok(()));
    }

    #[test]
    fn a_refused_frame_is_not_remembered_as_seen() {
        let guard = MeshReplayGuard::new(START_MS);
        let now = START_MS + 1_000_000;
        let envelope = envelope_created_at(now - WINDOW_MS - 1);
        assert_eq!(guard.admit(&envelope, now), Err(ReplayRejection::ClockSkew));
        assert_eq!(guard.admit(&envelope, now - 10), Ok(()));
    }

    #[test]
    fn rejections_from_one_source_are_reported_once_per_interval() {
        let guard = MeshReplayGuard::new(START_MS);
        let noisy = [0x01u8; NODE_ID_LEN];
        let other = [0x02u8; NODE_ID_LEN];
        assert!(guard.should_report(&noisy));
        assert!(!guard.should_report(&noisy));
        assert!(guard.should_report(&other));
    }
}
