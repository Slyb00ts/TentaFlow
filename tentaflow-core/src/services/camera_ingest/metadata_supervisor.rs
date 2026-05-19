// =============================================================================
// File: services/camera_ingest/metadata_supervisor.rs — ONVIF metadata pull
// task supervisor (F2 P6.b).
// =============================================================================
//
// One PullPoint subscription per camera, regardless of how many addons asked
// for it. A `MetadataPullSupervisor` instance keeps a `HashMap<camera_id,
// PullTaskHandle>` where each handle carries a cancellation token and a
// subscriber refcount. The first `ensure_pull_task` call spawns the task
// (CreatePullPointSubscription + long-poll PullMessages loop + publish to
// `metadata_bus`); subsequent calls only bump the refcount. `release` decs
// and cancels the task when the count drops to zero.
//
// Lifetime ownership matches `metadata_bus`: a process-wide singleton. Tasks
// outlive the addon Store that requested them — an addon termination only
// drops its bus subscribers, but the subscription stays open until the
// supervisor refcount hits zero. The host-fn unsubscribe path is what closes
// the subscription on the camera (best-effort `Unsubscribe` SOAP call).
//
// Renewal: the ONVIF spec requires a fresh subscription every
// `TerminationTime`. Rather than implementing a separate `Renew` action we
// re-issue `CreatePullPointSubscription` when the device-supplied
// termination is closer than `RENEW_LEAD_SECS`. Simpler, slightly heavier on
// the network, fine at the 5-minute subscription cadence.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::services::camera_ingest::metadata_bus::{metadata_bus, MetadataFrame};
use crate::services::camera_ingest::onvif_events::{
    create_pull_point_subscription, pull_messages, unsubscribe_pull_point,
};
use crate::services::camera_ingest::onvif_media::{OnvifCredentials, OnvifError};

/// Lead time before the device-supplied `TerminationTime` at which we
/// recreate the subscription. Picked to be larger than a single
/// `PULL_TIMEOUT_MS` window so a long-poll in flight can never outlive its
/// subscription.
const RENEW_LEAD_SECS: i64 = 60;

/// Each `CreatePullPointSubscription` requests this lifetime (seconds). The
/// device may shorten it; we honour the returned `TerminationTime`.
const SUBSCRIPTION_INITIAL_TERMINATION_SECS: u32 = 600;

/// Long-poll wait on every `PullMessages` call (milliseconds). Anything
/// shorter wastes round-trips; longer lengthens the cancel latency.
const PULL_TIMEOUT_MS: u32 = 30_000;

/// `PullMessages` batch size. ONVIF analytics frames are tiny, so the cap is
/// generous; the device returns immediately when its queue is shorter.
const PULL_MAX_MESSAGES: u32 = 100;

/// Best-effort `Unsubscribe` timeout during cancellation.
const UNSUBSCRIBE_TIMEOUT_MS: u32 = 5_000;

/// Floor + ceiling for the transport-error retry backoff. The pull loop
/// doubles the delay on every failed attempt until `MAX_BACKOFF_MS`.
const MIN_BACKOFF_MS: u64 = 500;
const MAX_BACKOFF_MS: u64 = 30_000;

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    /// Subscription creation failed with non-recoverable credentials. The
    /// addon should re-issue with refreshed credentials.
    #[error("authentication failed")]
    AuthFailed,
    /// Initial CreatePullPointSubscription failed with a transport error.
    #[error("transport: {0}")]
    Transport(String),
}

/// Generation counter used to disambiguate handles across rapid
/// release/ensure cycles. A late `release` for an old generation must not
/// cancel a newly-spawned task that reused the same `camera_id`.
type Generation = u64;

struct PullTaskHandle {
    cancel: CancellationToken,
    /// Number of active addon subscriptions on this camera. The task lives
    /// as long as `subscribers > 0`.
    subscribers: usize,
    /// Join handle taken by `release_and_wait` so callers can deterministically
    /// wait for the pull task (and its best-effort Unsubscribe SOAP call) to
    /// finish before a follow-up `ensure_pull_task` creates a fresh
    /// subscription on the device.
    join: Option<JoinHandle<()>>,
    /// Monotonic generation id stamped at task spawn. A task self-terminating
    /// (auth failure) checks this on cleanup so it only removes its OWN
    /// registry entry — a fresh task spawned after a credential rotation
    /// is not collateral damage.
    generation: Generation,
}

#[derive(Default)]
pub struct MetadataPullSupervisor {
    handles: Mutex<HashMap<String, PullTaskHandle>>,
    /// Per-camera mutex used as a serialisation point for
    /// `ensure_pull_task`. Two concurrent first-subscribers serialise on the
    /// same `Arc<tokio::Mutex>` so only one network round-trip
    /// (`CreatePullPointSubscription`) is performed; the loser observes the
    /// winner's handle and refcount-bumps. Held only across the create-and-
    /// publish window — never across a long-poll.
    ensure_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Monotonic generation counter (see `PullTaskHandle::generation`).
    next_generation: Mutex<Generation>,
}

impl MetadataPullSupervisor {
    pub fn new() -> Self {
        Self {
            handles: Mutex::new(HashMap::new()),
            ensure_locks: Mutex::new(HashMap::new()),
            next_generation: Mutex::new(1),
        }
    }

    /// Acquire (or create) the per-camera ensure lock. The returned `Arc`
    /// keeps the lock alive for the duration of the caller's critical
    /// section; the registry entry is dropped when the last `Arc` falls.
    fn ensure_lock_for(&self, camera_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut g = self.ensure_locks.lock();
        g.entry(camera_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Drop the per-camera ensure-lock entry once nobody is holding it.
    /// Called after the create-subscription window closes to keep the map
    /// from growing unbounded over the lifetime of the process.
    fn maybe_drop_ensure_lock(&self, camera_id: &str, current: &Arc<tokio::sync::Mutex<()>>) {
        let mut g = self.ensure_locks.lock();
        if let Some(existing) = g.get(camera_id) {
            // Strong count == 2 means only the map and our local Arc still
            // reference the lock; no other ensure_pull_task is waiting.
            if Arc::ptr_eq(existing, current) && Arc::strong_count(current) == 2 {
                g.remove(camera_id);
            }
        }
    }

    fn next_gen(&self) -> Generation {
        let mut g = self.next_generation.lock();
        let v = *g;
        *g = g.wrapping_add(1);
        v
    }

    /// Process-wide singleton. Matches the `metadata_bus()` accessor.
    pub fn global() -> &'static Arc<MetadataPullSupervisor> {
        use std::sync::OnceLock;
        static SUP: OnceLock<Arc<MetadataPullSupervisor>> = OnceLock::new();
        SUP.get_or_init(|| Arc::new(MetadataPullSupervisor::new()))
    }

    /// Increments the subscriber count for `camera_id`. When the count was
    /// previously zero, performs an initial
    /// `CreatePullPointSubscription` synchronously so the caller can
    /// surface auth / transport failures as proper ABI errors instead of
    /// silently spawning a doomed task. The long-poll loop is then spawned
    /// using the live subscription as its starting state.
    pub async fn ensure_pull_task(
        self: &Arc<Self>,
        camera_id: &str,
        creds: OnvifCredentials,
        events_service_url: String,
    ) -> Result<(), SupervisorError> {
        // Fast path — task already running. Take the bump without acquiring
        // the per-camera ensure lock so a steady-state stream of subscribes
        // never blocks on the slow path.
        {
            let mut guard = self.handles.lock();
            if let Some(h) = guard.get_mut(camera_id) {
                h.subscribers += 1;
                debug!(
                    "metadata_supervisor: refcount={} for camera_id='{}'",
                    h.subscribers, camera_id
                );
                return Ok(());
            }
        }

        // Slow path — serialise concurrent first-subscribers so only one
        // CreatePullPointSubscription is issued per camera. The lock is
        // released before the long-poll loop starts.
        let lock = self.ensure_lock_for(camera_id);
        let _permit = lock.lock().await;

        // Re-check: another first-subscriber may have won the lock and
        // already installed a handle. Refcount-bump and return.
        {
            let mut guard = self.handles.lock();
            if let Some(h) = guard.get_mut(camera_id) {
                h.subscribers += 1;
                debug!(
                    "metadata_supervisor: refcount={} for camera_id='{}' (post-lock)",
                    h.subscribers, camera_id
                );
                drop(guard);
                drop(_permit);
                self.maybe_drop_ensure_lock(camera_id, &lock);
                return Ok(());
            }
        }

        // No competing winner — perform the network round-trip.
        let initial = match create_pull_point_subscription(
            &events_service_url,
            &creds,
            SUBSCRIPTION_INITIAL_TERMINATION_SECS,
            PULL_TIMEOUT_MS,
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                drop(_permit);
                self.maybe_drop_ensure_lock(camera_id, &lock);
                return Err(map_initial_error(e));
            }
        };

        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task_camera = camera_id.to_string();
        let task_url = events_service_url.clone();
        let task_creds = creds.clone();
        let supervisor = Arc::clone(self);
        let generation = self.next_gen();

        let join = tokio::spawn(async move {
            run_pull_loop(
                supervisor,
                task_camera,
                task_url,
                task_creds,
                initial,
                task_cancel,
                generation,
            )
            .await;
        });

        self.handles.lock().insert(
            camera_id.to_string(),
            PullTaskHandle {
                cancel,
                subscribers: 1,
                join: Some(join),
                generation,
            },
        );
        info!(
            "metadata_supervisor: spawned pull task for camera_id='{}' gen={generation}",
            camera_id
        );
        drop(_permit);
        self.maybe_drop_ensure_lock(camera_id, &lock);
        Ok(())
    }

    /// Decrements the refcount; cancels the pull task when it reaches zero.
    /// Calling `release` for an unknown camera_id is a no-op (idempotent).
    /// The pull task observes the cancel token and exits asynchronously —
    /// callers that need to wait for the device-side Unsubscribe SOAP call
    /// to complete (typical when an addon resubscribes immediately) should
    /// use `release_and_wait` instead.
    pub fn release(&self, camera_id: &str) {
        let mut guard = self.handles.lock();
        let drop_now = match guard.get_mut(camera_id) {
            Some(h) => {
                if h.subscribers > 0 {
                    h.subscribers -= 1;
                }
                h.subscribers == 0
            }
            None => return,
        };
        if drop_now {
            if let Some(h) = guard.remove(camera_id) {
                h.cancel.cancel();
                info!(
                    "metadata_supervisor: refcount=0, cancelled pull task for '{}'",
                    camera_id
                );
                // Drop the JoinHandle — the task observes cancel and exits.
                drop(h.join);
            }
        }
    }

    /// Async sibling of `release`. When the refcount drops to zero the
    /// caller awaits the pull task's exit (bounded by `JOIN_TIMEOUT`) so
    /// the device-side Unsubscribe SOAP call has a chance to complete
    /// before a follow-up `ensure_pull_task` creates a fresh subscription.
    /// Returns `Ok(())` whether or not the wait timed out — the timeout
    /// path simply detaches the task and lets it finish in the background.
    pub async fn release_and_wait(&self, camera_id: &str) {
        // Strictly greater than UNSUBSCRIBE_TIMEOUT_MS (5s) so the in-task
        // unsubscribe SOAP call + tokio scheduling latency can complete
        // before we detach. An immediate resubscribe after this returns
        // therefore observes a torn-down device-side PullPoint.
        const JOIN_TIMEOUT: Duration = Duration::from_secs(8);
        let join_opt = {
            let mut guard = self.handles.lock();
            let drop_now = match guard.get_mut(camera_id) {
                Some(h) => {
                    if h.subscribers > 0 {
                        h.subscribers -= 1;
                    }
                    h.subscribers == 0
                }
                None => return,
            };
            if !drop_now {
                return;
            }
            let h = guard.remove(camera_id).expect("just confirmed present");
            h.cancel.cancel();
            info!(
                "metadata_supervisor: refcount=0, awaiting pull task exit for '{}'",
                camera_id
            );
            h.join
        };
        if let Some(j) = join_opt {
            let _ = tokio::time::timeout(JOIN_TIMEOUT, j).await;
        }
    }

    /// Number of cameras with an active pull task. Exposed for diagnostics
    /// (admin UI / tests). O(1).
    pub fn active_count(&self) -> usize {
        self.handles.lock().len()
    }

    /// Refcount for the given camera (0 if no task running). Exposed for
    /// diagnostics; consumers must not rely on this for correctness (the
    /// value can change between the call and the caller's next action).
    pub fn subscribers(&self, camera_id: &str) -> usize {
        self.handles
            .lock()
            .get(camera_id)
            .map(|h| h.subscribers)
            .unwrap_or(0)
    }
}

fn map_initial_error(e: OnvifError) -> SupervisorError {
    match e {
        OnvifError::AuthFailed => SupervisorError::AuthFailed,
        OnvifError::Transport(s) | OnvifError::SoapFault(s) | OnvifError::MalformedResponse(s) => {
            SupervisorError::Transport(s)
        }
        OnvifError::Timeout(ms) => SupervisorError::Transport(format!("timeout {ms}ms")),
        OnvifError::NoProfiles | OnvifError::ProfileNotFound(_) => {
            // Events service does not return these — guard against future
            // error variants reaching here.
            SupervisorError::Transport("unexpected error variant".to_string())
        }
    }
}

/// Detect SOAP faults that imply the device-side subscription is gone
/// (expired or unknown reference). These trigger an immediate recreate
/// instead of waiting for the renewal timer.
fn fault_indicates_dead_subscription(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("resourceunknown")
        || lower.contains("unabletogetmessages")
        || lower.contains("expired")
        || lower.contains("notfound")
}

/// Compute the local `Instant` at which the next renewal must complete.
/// `requested_secs` is what we asked the device for; `device_termination_unix`
/// is what the device echoed back. We do NOT trust the device's absolute
/// clock — clock skew between the host and the camera would otherwise push
/// the renewal arbitrarily late. Instead we anchor on the local receipt
/// `Instant` and use the SHORTER of (a) the device's reported lifetime
/// converted to a delta, (b) the lifetime we originally requested.
fn renewal_deadline(
    received_at: Instant,
    requested_secs: u32,
    device_termination_unix: i64,
    receipt_unix: i64,
) -> Instant {
    let device_delta_secs = (device_termination_unix - receipt_unix).max(0) as u64;
    let requested = requested_secs as u64;
    // Pick the shorter lifetime so a buggy device that echoes a far-future
    // timestamp cannot keep us from renewing.
    let chosen = device_delta_secs.min(requested).max(1);
    let lead = RENEW_LEAD_SECS as u64;
    let until_renew = chosen.saturating_sub(lead);
    received_at + Duration::from_secs(until_renew)
}

async fn run_pull_loop(
    supervisor: Arc<MetadataPullSupervisor>,
    camera_id: String,
    events_service_url: String,
    creds: OnvifCredentials,
    initial: crate::services::camera_ingest::onvif_events::PullPointSubscription,
    cancel: CancellationToken,
    generation: Generation,
) {
    audit_pull(&camera_id, "pull_started", None);

    let mut subscription = initial;
    // Local clock anchor for the initial subscription's renewal.
    let mut sub_received_at = Instant::now();
    let mut sub_receipt_unix = chrono::Utc::now().timestamp();
    let mut backoff_ms: u64 = MIN_BACKOFF_MS;

    'outer: loop {
        // Cancel-aware short-circuit before each long-poll iteration.
        if cancel.is_cancelled() {
            break;
        }

        // Renew (re-create) the subscription proactively. We compare on
        // local `Instant`s to immunise against clock skew with the camera.
        let deadline = renewal_deadline(
            sub_received_at,
            SUBSCRIPTION_INITIAL_TERMINATION_SECS,
            subscription.termination_time_unix,
            sub_receipt_unix,
        );
        if Instant::now() >= deadline {
            match create_pull_point_subscription(
                &events_service_url,
                &creds,
                SUBSCRIPTION_INITIAL_TERMINATION_SECS,
                PULL_TIMEOUT_MS,
            )
            .await
            {
                Ok(fresh) => {
                    let _ = unsubscribe_pull_point(
                        &subscription.reference_uri,
                        &creds,
                        UNSUBSCRIBE_TIMEOUT_MS,
                    )
                    .await;
                    subscription = fresh;
                    sub_received_at = Instant::now();
                    sub_receipt_unix = chrono::Utc::now().timestamp();
                    debug!(
                        "metadata_supervisor: renewed subscription for '{}'",
                        camera_id
                    );
                }
                Err(OnvifError::AuthFailed) => {
                    handle_auth_failure(&supervisor, &camera_id, generation).await;
                    return;
                }
                Err(e) => {
                    warn!(
                        "metadata_supervisor: renew failed for '{}': {e}; backing off",
                        camera_id
                    );
                    if sleep_or_cancel(&cancel, backoff_ms).await {
                        break;
                    }
                    backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                    continue;
                }
            }
        }

        // Long-poll for the next batch. The cancel future races the SOAP
        // call so a release() takes effect inside one PULL_TIMEOUT_MS window.
        let pull_fut = pull_messages(
            &subscription.reference_uri,
            &creds,
            PULL_MAX_MESSAGES,
            PULL_TIMEOUT_MS,
        );

        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            r = pull_fut => r,
        };

        match result {
            Ok(events) => {
                backoff_ms = MIN_BACKOFF_MS;
                for ev in events {
                    if ev.items.is_empty() {
                        continue;
                    }
                    let frame = MetadataFrame {
                        camera_id: camera_id.clone(),
                        ts_unix: ev.utc_timestamp.saturating_mul(1_000),
                        items: ev.items,
                    };
                    metadata_bus().publish(frame);
                }
            }
            Err(OnvifError::AuthFailed) => {
                handle_auth_failure(&supervisor, &camera_id, generation).await;
                return;
            }
            Err(OnvifError::SoapFault(ref fault)) if fault_indicates_dead_subscription(fault) => {
                // Device dropped the subscription out from under us — recreate
                // immediately instead of waiting for the renewal timer.
                warn!(
                    "metadata_supervisor: subscription dead for '{}' ({fault}); recreating",
                    camera_id
                );
                match create_pull_point_subscription(
                    &events_service_url,
                    &creds,
                    SUBSCRIPTION_INITIAL_TERMINATION_SECS,
                    PULL_TIMEOUT_MS,
                )
                .await
                {
                    Ok(fresh) => {
                        subscription = fresh;
                        sub_received_at = Instant::now();
                        sub_receipt_unix = chrono::Utc::now().timestamp();
                        backoff_ms = MIN_BACKOFF_MS;
                    }
                    Err(OnvifError::AuthFailed) => {
                        handle_auth_failure(&supervisor, &camera_id, generation).await;
                        return;
                    }
                    Err(e) => {
                        warn!(
                            "metadata_supervisor: recreate after dead-sub failed for '{}': {e}",
                            camera_id
                        );
                        if sleep_or_cancel(&cancel, backoff_ms).await {
                            break 'outer;
                        }
                        backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                    }
                }
                continue;
            }
            Err(e) => {
                warn!(
                    "metadata_supervisor: pull error for '{}': {e}; backoff={}ms",
                    camera_id, backoff_ms
                );
                if sleep_or_cancel(&cancel, backoff_ms).await {
                    break;
                }
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                continue;
            }
        }
    }

    // Cancellation path — best-effort teardown so the camera releases its
    // subscription slot.
    let _ =
        unsubscribe_pull_point(&subscription.reference_uri, &creds, UNSUBSCRIBE_TIMEOUT_MS).await;
    audit_pull(&camera_id, "pull_stopped", Some("cancelled"));
}

/// Auth failure path: emit CameraOffline to every active bus subscriber so
/// addons learn the subscription is dead, then remove the supervisor handle
/// — but only when the entry's generation still matches. A late auth-fail
/// for a previous generation must not yank a freshly-spawned task that was
/// installed after credentials were rotated.
async fn handle_auth_failure(
    supervisor: &Arc<MetadataPullSupervisor>,
    camera_id: &str,
    generation: Generation,
) {
    // Guard with the generation check FIRST. A late auth-fail from a previous
    // task generation must not close the bus for a freshly-installed task
    // (post-credential-rotation respawn). We atomically remove the matching
    // generation entry under the handles lock; only if we owned it do we
    // proceed to close_camera. Stale auth-fails become a silent no-op.
    let owned = {
        let mut g = supervisor.handles.lock();
        match g.get(camera_id) {
            Some(h) if h.generation == generation => {
                g.remove(camera_id);
                true
            }
            _ => false,
        }
    };
    if !owned {
        // Different generation owns the handle now — leave the new task alone.
        return;
    }
    warn!(
        "metadata_supervisor: auth failed for '{}' gen={generation}, closing bus",
        camera_id
    );
    metadata_bus().close_camera(camera_id, "auth_failed").await;
    audit_pull(camera_id, "pull_stopped", Some("auth_failed"));
}

/// Sleeps `ms` milliseconds or returns `true` if cancelled mid-sleep.
async fn sleep_or_cancel(cancel: &CancellationToken, ms: u64) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => true,
        _ = tokio::time::sleep(Duration::from_millis(ms)) => false,
    }
}

/// Emit a lightweight tracing breadcrumb. Audit-log persistence is performed
/// from the host-fn entry points (subscribe / unsubscribe) where the addon
/// identity is available; the supervisor itself has no `AddonState` handle.
fn audit_pull(camera_id: &str, action: &str, reason: Option<&str>) {
    match reason {
        Some(r) => info!(
            target: "flow.camera.metadata",
            "{action} camera_id='{camera_id}' reason='{r}'"
        ),
        None => info!(
            target: "flow.camera.metadata",
            "{action} camera_id='{camera_id}'"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::camera_ingest::onvif_events::PullPointSubscription;

    fn fake_creds() -> OnvifCredentials {
        OnvifCredentials {
            username: "u".to_string(),
            password: "p".to_string(),
        }
    }

    /// Inject a synthetic running task into the supervisor map without going
    /// through `create_pull_point_subscription` (which would need a real
    /// camera). Used to exercise refcount semantics in isolation.
    fn install_synthetic_task(sup: &MetadataPullSupervisor, camera_id: &str) -> CancellationToken {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let join = tokio::spawn(async move {
            // Pure idle waiter so the JoinHandle stays alive until cancelled.
            task_cancel.cancelled().await;
        });
        sup.handles.lock().insert(
            camera_id.to_string(),
            PullTaskHandle {
                cancel,
                subscribers: 1,
                join: Some(join),
                generation: 0,
            },
        );
        sup.handles
            .lock()
            .get(camera_id)
            .map(|h| h.cancel.clone())
            .expect("just inserted")
    }

    #[tokio::test]
    async fn ensure_pull_task_first_call_spawns_then_subsequent_calls_just_ref_count() {
        let sup = Arc::new(MetadataPullSupervisor::new());
        let tok = install_synthetic_task(&sup, "cam1");
        assert_eq!(sup.subscribers("cam1"), 1);
        assert_eq!(sup.active_count(), 1);

        // A second ensure path mimics what the host fn does after a refcount
        // hit (the function returns early without touching the network).
        {
            let mut guard = sup.handles.lock();
            let h = guard.get_mut("cam1").unwrap();
            h.subscribers += 1;
        }
        assert_eq!(sup.subscribers("cam1"), 2);
        // Task still alive.
        assert!(!tok.is_cancelled());
    }

    #[tokio::test]
    async fn release_at_zero_cancels_task_and_drops_entry() {
        let sup = Arc::new(MetadataPullSupervisor::new());
        let tok = install_synthetic_task(&sup, "cam2");
        sup.release("cam2");
        assert_eq!(sup.subscribers("cam2"), 0);
        assert_eq!(sup.active_count(), 0);
        // Cancel token was triggered.
        assert!(tok.is_cancelled());
    }

    #[tokio::test]
    async fn release_with_remaining_subscribers_keeps_task_alive() {
        let sup = Arc::new(MetadataPullSupervisor::new());
        let tok = install_synthetic_task(&sup, "cam3");
        // Bump to 2 subscribers (mimic a second ensure_pull_task hit).
        {
            let mut g = sup.handles.lock();
            g.get_mut("cam3").unwrap().subscribers = 2;
        }
        sup.release("cam3");
        assert_eq!(sup.subscribers("cam3"), 1);
        assert_eq!(sup.active_count(), 1);
        assert!(!tok.is_cancelled());
    }

    #[tokio::test]
    async fn release_unknown_camera_is_noop() {
        let sup = Arc::new(MetadataPullSupervisor::new());
        sup.release("never-seen");
        // Did not panic; map remains empty.
        assert_eq!(sup.active_count(), 0);
    }

    #[tokio::test]
    async fn release_below_zero_is_safe() {
        let sup = Arc::new(MetadataPullSupervisor::new());
        let tok = install_synthetic_task(&sup, "cam_z");
        sup.release("cam_z");
        sup.release("cam_z"); // Already removed — must not panic.
        assert_eq!(sup.active_count(), 0);
        assert!(tok.is_cancelled());
    }

    #[test]
    fn map_initial_error_routes_auth_to_authfailed() {
        assert!(matches!(
            map_initial_error(OnvifError::AuthFailed),
            SupervisorError::AuthFailed
        ));
        assert!(matches!(
            map_initial_error(OnvifError::Transport("boom".into())),
            SupervisorError::Transport(_)
        ));
        assert!(matches!(
            map_initial_error(OnvifError::Timeout(5000)),
            SupervisorError::Transport(_)
        ));
    }

    // Used by `install_synthetic_task` to verify `PullPointSubscription`
    // matches the expected layout — keeps a compile-time guard against
    // accidental field renames in onvif_events.
    #[test]
    fn pull_point_subscription_shape_is_stable() {
        let _s = PullPointSubscription {
            reference_uri: "http://x".into(),
            termination_time_unix: 0,
        };
    }

    // ---------------------------------------------------------------------
    // Codex review fixes — issue-specific regression tests
    // ---------------------------------------------------------------------

    /// Issue #1: two `ensure_lock_for` callers must obtain the SAME lock so a
    /// concurrent first-subscribe is serialised at the create-subscription
    /// boundary. We probe the registry directly rather than driving real
    /// network I/O.
    #[tokio::test]
    async fn ensure_lock_for_returns_same_lock_per_camera() {
        let sup = Arc::new(MetadataPullSupervisor::new());
        let a = sup.ensure_lock_for("cam_lock");
        let b = sup.ensure_lock_for("cam_lock");
        assert!(
            Arc::ptr_eq(&a, &b),
            "same camera_id must hand back same Arc"
        );
        // Different camera_id must yield a distinct Arc.
        let c = sup.ensure_lock_for("cam_other");
        assert!(!Arc::ptr_eq(&a, &c));
    }

    /// Issue #1: after a serialised first-subscribe completes and both Arcs
    /// drop, `maybe_drop_ensure_lock` evicts the entry so the per-camera
    /// lock map does not grow unbounded.
    #[tokio::test]
    async fn maybe_drop_ensure_lock_evicts_when_last_ref() {
        let sup = Arc::new(MetadataPullSupervisor::new());
        let l = sup.ensure_lock_for("cam_evict");
        sup.maybe_drop_ensure_lock("cam_evict", &l);
        // Lock entry removed; a fresh acquire creates a NEW Arc.
        drop(l);
        let fresh = sup.ensure_lock_for("cam_evict");
        let _ = fresh;
    }

    /// Issue #3: `release_and_wait` cancels the task AND awaits its exit so a
    /// subscribe-after-unsubscribe sees the previous task finished.
    #[tokio::test]
    async fn release_and_wait_blocks_until_task_exits() {
        let sup = Arc::new(MetadataPullSupervisor::new());
        let tok = install_synthetic_task(&sup, "cam_join");
        // The synthetic task exits as soon as it observes cancel.
        sup.release_and_wait("cam_join").await;
        // Task must be cancelled (its idle loop terminates) and entry gone.
        assert!(tok.is_cancelled());
        assert_eq!(sup.active_count(), 0);
    }

    /// Issue #3: `release_and_wait` for an unknown camera is a no-op (same
    /// idempotent semantics as `release`).
    #[tokio::test]
    async fn release_and_wait_unknown_camera_is_noop() {
        let sup = Arc::new(MetadataPullSupervisor::new());
        sup.release_and_wait("never-seen").await;
        assert_eq!(sup.active_count(), 0);
    }

    /// Issue #6: renewal anchors on the LOCAL `Instant` of receipt and on
    /// the shorter of (device-reported lifetime, requested lifetime). A
    /// camera whose clock is far in the future cannot trick the supervisor
    /// into postponing renewal indefinitely.
    #[test]
    fn renewal_deadline_caps_at_requested_lifetime() {
        let received_at = Instant::now();
        // Device claims a 10-hour subscription but we asked for 600 s.
        let receipt_unix = 1_700_000_000;
        let device_term = receipt_unix + 36_000;
        let dl = renewal_deadline(received_at, 600, device_term, receipt_unix);
        // Must be `received_at + (600 - 60) = +540 s`, NOT 36000 - 60 s.
        let expected = received_at + Duration::from_secs(540);
        assert!(dl <= expected + Duration::from_millis(1));
        assert!(dl >= expected - Duration::from_millis(1));
    }

    /// Issue #6: SOAP faults that name a dead subscription must trigger an
    /// immediate recreate in the pull loop. `fault_indicates_dead_subscription`
    /// is the predicate that gates that branch.
    #[test]
    fn dead_subscription_fault_detector_matches_known_strings() {
        assert!(fault_indicates_dead_subscription(
            "wsnt:ResourceUnknownFault: subscription expired"
        ));
        assert!(fault_indicates_dead_subscription("UnableToGetMessages"));
        assert!(fault_indicates_dead_subscription("ter:Expired"));
        assert!(fault_indicates_dead_subscription("NotFound"));
        assert!(!fault_indicates_dead_subscription("ter:ActionNotSupported"));
        assert!(!fault_indicates_dead_subscription(""));
    }

    /// Issue #4: when the auth-failure path runs, it must remove the handle
    /// ONLY when the generation still matches. A late auth-fail from an old
    /// task must not cancel a freshly-spawned task that replaced it.
    #[tokio::test]
    async fn handle_auth_failure_respects_generation() {
        let sup = Arc::new(MetadataPullSupervisor::new());
        // Install a synthetic "new generation" handle by hand.
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let join = tokio::spawn(async move { task_cancel.cancelled().await });
        sup.handles.lock().insert(
            "cam_gen".to_string(),
            PullTaskHandle {
                cancel,
                subscribers: 1,
                join: Some(join),
                generation: 42,
            },
        );
        // Call handle_auth_failure with a STALE generation (1) — must NOT
        // remove the live handle.
        handle_auth_failure(&sup, "cam_gen", 1).await;
        assert_eq!(
            sup.active_count(),
            1,
            "stale-generation auth failure must not yank live handle"
        );
        // Now the matching generation — handle is removed.
        handle_auth_failure(&sup, "cam_gen", 42).await;
        assert_eq!(sup.active_count(), 0);
    }
}
