//! Bounds the peer commands the mesh event loop runs.
//!
//! Each command runs in its own task (a command may ask its sender something,
//! and the answer comes through the event loop). That must not become an
//! unbounded fan-out a peer can drive: a peer has `PER_PEER` commands in
//! flight, the node `GLOBAL`, and one more is answered "busy" at once instead
//! of being queued. The tasks end with the loop that started them, and a peer's
//! end when it disconnects or loses trust.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// Commands one peer may have in flight.
pub const PER_PEER: usize = 32;
/// Commands in flight from all peers together.
pub const GLOBAL: usize = 256;

/// The permits and the cancellation of one running command.
pub struct CommandSlot {
    pub cancel: CancellationToken,
    _peer: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
}

struct PeerSlots {
    permits: Arc<Semaphore>,
    cancel: CancellationToken,
}

pub struct CommandTasks {
    global: Arc<Semaphore>,
    per_peer_limit: usize,
    peers: DashMap<String, PeerSlots>,
    root: CancellationToken,
}

impl CommandTasks {
    pub fn new() -> Self {
        Self::with_limits(PER_PEER, GLOBAL)
    }

    pub fn with_limits(per_peer: usize, global: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global)),
            per_peer_limit: per_peer,
            peers: DashMap::new(),
            root: CancellationToken::new(),
        }
    }

    /// A slot for one more command from `peer`, or `None` when the peer or
    /// the node is at its limit.
    pub fn try_start(&self, peer: &str) -> Option<CommandSlot> {
        let (permits, cancel) = {
            let mut slots = self
                .peers
                .entry(peer.to_string())
                .or_insert_with(|| PeerSlots {
                    permits: Arc::new(Semaphore::new(self.per_peer_limit)),
                    cancel: self.root.child_token(),
                });
            if slots.cancel.is_cancelled() {
                slots.permits = Arc::new(Semaphore::new(self.per_peer_limit));
                slots.cancel = self.root.child_token();
            }
            (slots.permits.clone(), slots.cancel.clone())
        };
        let peer_permit = permits.try_acquire_owned().ok()?;
        let global_permit = self.global.clone().try_acquire_owned().ok()?;
        Some(CommandSlot {
            cancel,
            _peer: peer_permit,
            _global: global_permit,
        })
    }

    /// Cancels what `peer` has in flight: it disconnected or lost trust.
    pub fn cancel_peer(&self, peer: &str) {
        if let Some(slots) = self.peers.get(peer) {
            slots.cancel.cancel();
        }
    }
}

impl Default for CommandTasks {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CommandTasks {
    fn drop(&mut self) {
        self.root.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_peer_at_its_limit_is_refused_while_another_is_served() {
        let tasks = CommandTasks::with_limits(2, 10);
        let a = (tasks.try_start("a"), tasks.try_start("a"));
        assert!(a.0.is_some() && a.1.is_some());
        assert!(tasks.try_start("a").is_none(), "a third command from a");
        assert!(tasks.try_start("b").is_some(), "b is not held back by a");
    }

    #[test]
    fn the_node_wide_limit_holds_across_peers() {
        let tasks = CommandTasks::with_limits(5, 2);
        let held = (tasks.try_start("a"), tasks.try_start("b"));
        assert!(held.0.is_some() && held.1.is_some());
        assert!(tasks.try_start("c").is_none());
        drop(held);
        assert!(
            tasks.try_start("c").is_some(),
            "a finished command frees its slot"
        );
    }

    #[test]
    fn cancelling_a_peer_stops_its_commands_and_leaves_the_next_ones_running() {
        let tasks = CommandTasks::with_limits(2, 10);
        let slot = tasks.try_start("a").unwrap();
        let other = tasks.try_start("b").unwrap();
        tasks.cancel_peer("a");
        assert!(slot.cancel.is_cancelled());
        assert!(!other.cancel.is_cancelled());
        let next = tasks.try_start("a").unwrap();
        assert!(
            !next.cancel.is_cancelled(),
            "a reconnected peer starts afresh"
        );
    }

    #[test]
    fn dropping_the_tasks_cancels_every_command() {
        let tasks = CommandTasks::with_limits(2, 10);
        let slot = tasks.try_start("a").unwrap();
        drop(tasks);
        assert!(slot.cancel.is_cancelled());
    }
}
