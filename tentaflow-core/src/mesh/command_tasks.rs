//! Bounds the peer commands the mesh event loop runs.
//!
//! Each command runs in its own task (a command may ask its sender something,
//! and the answer comes through the event loop). That must not become an
//! unbounded fan-out a peer can drive: a peer has `PER_PEER` commands in
//! flight, the node `GLOBAL`, and one more is answered "busy" at once instead
//! of being queued. A command runs to its end or to `COMMAND_TIMEOUT`; a peer
//! that disconnects does not stop it (its reply fails harmlessly), and a peer
//! that loses trust has the commands not yet started dropped and the replies
//! of the running ones withheld.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// Commands one peer may have in flight.
pub const PER_PEER: usize = 32;
/// Commands in flight from all peers together.
pub const GLOBAL: usize = 256;
/// Longest a short command may run (`command_timeout`).
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(600);
/// Longest any other command may run: model transfers and deployments wait
/// for hours (a model pull 2 h, a bulk model push 24 h on the sender's side),
/// and a bound shorter than the sender's wait makes them always fail.
pub const LONG_COMMAND_TIMEOUT: Duration = Duration::from_secs(25 * 3600);

/// How long the executor lets `command` run: the short bound only for the
/// kinds known to finish quickly, the sender's own longest wait for the rest.
pub fn command_timeout(command: &tentaflow_protocol::mesh::MeshCommandType) -> Duration {
    use tentaflow_protocol::mesh::MeshCommandType as C;
    match command {
        C::ContainerStart { .. }
        | C::ContainerStop { .. }
        | C::ContainerRestart { .. }
        | C::ListContainers
        | C::ListImages
        | C::BandwidthProbe { .. }
        | C::BandwidthProbeCancel
        | C::ProfilingStart(..)
        | C::ProfilingStop(..)
        | C::ProfilingSessions(..)
        | C::ProfilingReport(..)
        | C::ProfilingDelete(..)
        | C::ProfilingActiveInfo(..)
        | C::ServiceStartRemote { .. }
        | C::ServiceDeleteRemote { .. }
        | C::ServicePinRemote { .. }
        | C::ServicePauseRemote { .. } => COMMAND_TIMEOUT,
        _ => LONG_COMMAND_TIMEOUT,
    }
}
/// "Busy" replies in flight at once; more are dropped without an answer.
const BUSY_REPLIES: usize = 8;
/// Longest a "busy" reply may take to be written.
pub const BUSY_REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// How often one peer's refusals are logged.
const BUSY_WARN_EVERY: Duration = Duration::from_secs(60);
/// The peer map is swept of idle peers once it holds this many.
const SWEEP_AT: usize = 64;
/// Error code of a command refused for lack of a slot; the sender may retry
/// later, unlike after a failure of the command itself.
pub const BUSY_ERROR_CODE: &str = "busy";

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
    busy_replies: Arc<Semaphore>,
    per_peer_limit: usize,
    peers: DashMap<String, PeerSlots>,
    busy_warned: DashMap<String, Instant>,
    root: CancellationToken,
}

/// Runs `work` in its own task that owns `slot` to the end, so the slot's
/// permits are held exactly while the command runs.
pub fn spawn_holding<F>(slot: CommandSlot, work: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        work.await;
        drop(slot);
    });
}

impl CommandTasks {
    pub fn new() -> Self {
        Self::with_limits(PER_PEER, GLOBAL)
    }

    pub fn with_limits(per_peer: usize, global: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global)),
            busy_replies: Arc::new(Semaphore::new(BUSY_REPLIES)),
            per_peer_limit: per_peer,
            peers: DashMap::new(),
            busy_warned: DashMap::new(),
            root: CancellationToken::new(),
        }
    }

    /// A slot for one more command from `peer`, or `None` when the peer or
    /// the node is at its limit.
    pub fn try_start(&self, peer: &str) -> Option<CommandSlot> {
        if self.peers.len() >= SWEEP_AT {
            let limit = self.per_peer_limit;
            self.peers
                .retain(|_, slots| slots.permits.available_permits() < limit);
        }
        let (permits, cancel) = {
            let mut slots = self
                .peers
                .entry(peer.to_string())
                .or_insert_with(|| PeerSlots {
                    permits: Arc::new(Semaphore::new(self.per_peer_limit)),
                    cancel: self.root.child_token(),
                });
            // A peer that lost trust and came back starts with a fresh token but
            // keeps its permits: the commands it had running hold them until
            // they have stopped, so it cannot exceed its limit meanwhile.
            if slots.cancel.is_cancelled() {
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

    /// Cancels what `peer` has in flight: it lost trust.
    pub fn cancel_peer(&self, peer: &str) {
        if let Some(slots) = self.peers.get(peer) {
            slots.cancel.cancel();
        }
    }

    /// Permission to write one "busy" reply, or `None` when enough are being
    /// written already.
    pub fn busy_reply_permit(&self) -> Option<OwnedSemaphorePermit> {
        self.busy_replies.clone().try_acquire_owned().ok()
    }

    /// True once per `BUSY_WARN_EVERY` for `peer`: a peer at its limit is
    /// refused on every command, which must not be a log line each.
    pub fn should_warn_busy(&self, peer: &str) -> bool {
        let now = Instant::now();
        match self.busy_warned.entry(peer.to_string()) {
            dashmap::mapref::entry::Entry::Vacant(e) => {
                e.insert(now);
                true
            }
            dashmap::mapref::entry::Entry::Occupied(mut e) => {
                if now.saturating_duration_since(*e.get()) >= BUSY_WARN_EVERY {
                    e.insert(now);
                    true
                } else {
                    false
                }
            }
        }
    }

    #[cfg(test)]
    fn tracked_peers(&self) -> usize {
        self.peers.len()
    }
}

/// `work`'s output, or `None` when it did not finish within `limit`.
pub async fn run_bounded<F: Future>(limit: Duration, work: F) -> Option<F::Output> {
    tokio::time::timeout(limit, work).await.ok()
}

/// `run_bounded` that also stops, dropping `work`, when `cancel` fires: the
/// peer lost trust, or the node is shutting down.
pub async fn run_bounded_unless_cancelled<F: Future>(
    limit: Duration,
    cancel: &CancellationToken,
    work: F,
) -> Result<Option<F::Output>, Cancelled> {
    tokio::select! {
        out = run_bounded(limit, work) => Ok(out),
        _ = cancel.cancelled() => Err(Cancelled),
    }
}

/// A command stopped because its peer lost trust.
#[derive(Debug, PartialEq, Eq)]
pub struct Cancelled;

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

    #[tokio::test]
    async fn revoking_trust_stops_a_running_command() {
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            stop.cancel();
        });
        let out = tokio::time::timeout(
            Duration::from_secs(5),
            run_bounded_unless_cancelled(
                Duration::from_secs(3600),
                &cancel,
                std::future::pending::<()>(),
            ),
        )
        .await
        .expect("the running command was not stopped");
        assert_eq!(out, Err(Cancelled));
    }

    #[test]
    fn a_peer_that_lost_trust_cannot_exceed_its_limit_while_old_commands_still_run() {
        let tasks = CommandTasks::with_limits(1, 10);
        let old = tasks.try_start("a").unwrap();
        tasks.cancel_peer("a");
        assert!(
            tasks.try_start("a").is_none(),
            "the cancelled command still holds the peer's permit"
        );
        drop(old);
        let fresh = tasks.try_start("a").unwrap();
        assert!(!fresh.cancel.is_cancelled());
    }

    #[tokio::test]
    async fn dropping_the_tasks_cancels_what_they_run() {
        let tasks = CommandTasks::with_limits(2, 10);
        let slot = tasks.try_start("a").unwrap();
        let cancel = slot.cancel.clone();
        drop(tasks);
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn a_model_transfer_is_not_cut_off_by_the_short_bound() {
        use tentaflow_protocol::mesh::MeshCommandType as C;
        assert_eq!(command_timeout(&C::ListContainers), COMMAND_TIMEOUT);
        let ensure = C::EnsureModelLocal {
            deployment_cluster_id: "c".into(),
            model_repo: "m".into(),
            engine_id: "e".into(),
        };
        // The caller waits 7200 s for this one.
        assert!(command_timeout(&ensure) >= Duration::from_secs(7200));
        // And 24 h for a bulk push.
        assert!(
            command_timeout(&C::PushModelToPeer {
                deployment_cluster_id: "c".into(),
                model_repo: "m".into(),
                target_node_id: "n".into(),
            }) >= Duration::from_secs(24 * 3600)
        );
    }

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

    #[tokio::test]
    async fn a_spawned_command_keeps_its_slot_until_it_ends() {
        let tasks = CommandTasks::with_limits(1, 10);
        let slot = tasks.try_start("a").unwrap();
        let (end, ended) = tokio::sync::oneshot::channel::<()>();
        spawn_holding(slot, async move {
            let _ = ended.await;
        });
        tokio::task::yield_now().await;
        assert!(
            tasks.try_start("a").is_none(),
            "the running command still holds the peer's only slot"
        );
        end.send(()).unwrap();
        for _ in 0..50 {
            if tasks.try_start("a").is_some() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("the slot was not freed after the command ended");
    }

    #[tokio::test(start_paused = true)]
    async fn a_command_that_never_ends_is_cut_off_at_the_limit() {
        let outcome = tokio::time::timeout(
            Duration::from_secs(60),
            run_bounded(Duration::from_secs(5), std::future::pending::<()>()),
        )
        .await
        .expect("run_bounded did not cut the command off");
        assert!(outcome.is_none());
        let done = run_bounded(Duration::from_secs(5), async { 7 }).await;
        assert_eq!(done, Some(7));
    }

    #[test]
    fn idle_peers_are_swept_from_the_map_and_busy_ones_kept() {
        let tasks = CommandTasks::with_limits(2, 1000);
        let busy = tasks.try_start("busy").unwrap();
        for i in 0..SWEEP_AT {
            drop(tasks.try_start(&format!("p{i}")));
        }
        for i in 0..SWEEP_AT {
            drop(tasks.try_start(&format!("q{i}")));
        }
        assert!(
            tasks.tracked_peers() <= SWEEP_AT,
            "idle peers are swept, got {}",
            tasks.tracked_peers()
        );
        assert!(tasks.try_start("busy").is_some());
        drop(busy);
    }

    #[test]
    fn busy_replies_are_bounded_and_the_warning_is_rate_limited() {
        let tasks = CommandTasks::with_limits(1, 1);
        let held: Vec<_> = (0..BUSY_REPLIES)
            .map(|_| tasks.busy_reply_permit().unwrap())
            .collect();
        assert!(tasks.busy_reply_permit().is_none());
        drop(held);
        assert!(tasks.busy_reply_permit().is_some());
        assert!(tasks.should_warn_busy("a"));
        assert!(!tasks.should_warn_busy("a"));
        assert!(tasks.should_warn_busy("b"));
    }

    #[test]
    fn dropping_the_tasks_cancels_every_command() {
        let tasks = CommandTasks::with_limits(2, 10);
        let slot = tasks.try_start("a").unwrap();
        drop(tasks);
        assert!(slot.cancel.is_cancelled());
    }
}
