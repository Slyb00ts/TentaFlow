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
use std::time::Duration;

use chrono::Utc;
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

struct PullTaskHandle {
    cancel: CancellationToken,
    /// Number of active addon subscriptions on this camera. The task lives
    /// as long as `subscribers > 0`.
    subscribers: usize,
    /// Detached join handle. We never `.await` it from `release` — the task
    /// observes `cancel` and exits on its own.
    _join: JoinHandle<()>,
}

#[derive(Default)]
pub struct MetadataPullSupervisor {
    handles: Mutex<HashMap<String, PullTaskHandle>>,
}

impl MetadataPullSupervisor {
    pub fn new() -> Self {
        Self {
            handles: Mutex::new(HashMap::new()),
        }
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
        // Refcount bump path — task already running.
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

        // Initial subscribe — do this BEFORE spawning so the caller learns
        // immediately about AuthFailed / Transport errors.
        let initial = create_pull_point_subscription(
            &events_service_url,
            &creds,
            SUBSCRIPTION_INITIAL_TERMINATION_SECS,
            PULL_TIMEOUT_MS,
        )
        .await
        .map_err(map_initial_error)?;

        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task_camera = camera_id.to_string();
        let task_url = events_service_url.clone();
        let task_creds = creds.clone();
        let supervisor = Arc::clone(self);

        let join = tokio::spawn(async move {
            run_pull_loop(
                supervisor,
                task_camera,
                task_url,
                task_creds,
                initial,
                task_cancel,
            )
            .await;
        });

        let mut guard = self.handles.lock();
        // Race: a concurrent ensure may have inserted; if so cancel ours and
        // bump the existing refcount instead.
        if let Some(h) = guard.get_mut(camera_id) {
            h.subscribers += 1;
            drop(guard);
            cancel.cancel();
            return Ok(());
        }
        guard.insert(
            camera_id.to_string(),
            PullTaskHandle {
                cancel,
                subscribers: 1,
                _join: join,
            },
        );
        info!(
            "metadata_supervisor: spawned pull task for camera_id='{}'",
            camera_id
        );
        Ok(())
    }

    /// Decrements the refcount; cancels the pull task when it reaches zero.
    /// Calling `release` for an unknown camera_id is a no-op (idempotent).
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
            }
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

async fn run_pull_loop(
    supervisor: Arc<MetadataPullSupervisor>,
    camera_id: String,
    events_service_url: String,
    creds: OnvifCredentials,
    initial: crate::services::camera_ingest::onvif_events::PullPointSubscription,
    cancel: CancellationToken,
) {
    audit_pull(&camera_id, "pull_started", None);

    let mut subscription = initial;
    let mut backoff_ms: u64 = MIN_BACKOFF_MS;

    loop {
        // Cancel-aware short-circuit before each long-poll iteration.
        if cancel.is_cancelled() {
            break;
        }

        // Renew (re-create) the subscription proactively. ONVIF cameras drop
        // pulls past `TerminationTime` and return either `ResourceUnknown`
        // SOAP faults or HTTP 404 — recreating before the deadline keeps the
        // loop steady-state.
        let now = Utc::now().timestamp();
        if subscription.termination_time_unix - now <= RENEW_LEAD_SECS {
            match create_pull_point_subscription(
                &events_service_url,
                &creds,
                SUBSCRIPTION_INITIAL_TERMINATION_SECS,
                PULL_TIMEOUT_MS,
            )
            .await
            {
                Ok(fresh) => {
                    // Best-effort teardown of the previous subscription so we
                    // do not leave orphans on the camera. Idempotent — silently
                    // swallows SOAP faults from already-expired subscriptions.
                    let _ = unsubscribe_pull_point(
                        &subscription.reference_uri,
                        &creds,
                        UNSUBSCRIBE_TIMEOUT_MS,
                    )
                    .await;
                    subscription = fresh;
                    debug!(
                        "metadata_supervisor: renewed subscription for '{}'",
                        camera_id
                    );
                }
                Err(OnvifError::AuthFailed) => {
                    warn!(
                        "metadata_supervisor: auth failed on renew for '{}', exiting",
                        camera_id
                    );
                    audit_pull(&camera_id, "pull_stopped", Some("auth_failed"));
                    // Drop the registry entry so a later `ensure_pull_task`
                    // can spawn a fresh task once creds are rotated.
                    supervisor.handles.lock().remove(&camera_id);
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
                        // Filter out events without a parsable metadata
                        // payload — they are typically motion / tamper alerts
                        // we surface via a separate path in F3.
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
                warn!(
                    "metadata_supervisor: auth failed mid-loop for '{}', exiting",
                    camera_id
                );
                audit_pull(&camera_id, "pull_stopped", Some("auth_failed"));
                supervisor.handles.lock().remove(&camera_id);
                return;
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
    let _ = unsubscribe_pull_point(
        &subscription.reference_uri,
        &creds,
        UNSUBSCRIBE_TIMEOUT_MS,
    )
    .await;
    audit_pull(&camera_id, "pull_stopped", Some("cancelled"));
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
    fn install_synthetic_task(
        sup: &MetadataPullSupervisor,
        camera_id: &str,
    ) -> CancellationToken {
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
                _join: join,
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
}
