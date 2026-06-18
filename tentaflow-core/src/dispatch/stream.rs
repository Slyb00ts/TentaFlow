// =============================================================================
// File: dispatch/stream.rs
// Purpose: Binary stream pub/sub WS handlers (Chunk B). Wires the
//          `services::stream_hub::StreamHub` foundation into the dispatch
//          layer so a browser can subscribe to a hub-registered stream over
//          the binary WS protocol and receive an init segment + media frames
//          on the same correlation id. Pairs with a synchronous close handler
//          that cancels the active subscription for this WS connection.
// =============================================================================

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth, StreamClosedPayload,
    StreamFramePayload, StreamPayload, StreamSubscribeResponse,
};
use tokio::sync::broadcast::error::RecvError;

use super::subscription::{
    push_chunk_async, push_end, push_end_async, StreamHandlerMeta, Subscription,
};
use super::{HandlerContext, SessionAuthKind};
use crate::services::stream_hub::{StreamHub, StreamHubError};

/// Stream-id prefix gating tier. Today only `camera:` is wired; future
/// prefixes (`audio:`, `screen:`) get their own permission check below.
const CAMERA_PREFIX: &str = "camera:";
/// Permission required for `camera:` stream ids.
const PERM_CAMERA_READ: &str = "camera.read";

/// Hard ceiling on concurrent stream subscriptions per authenticated user.
/// Beyond this the handler rejects with `QuotaExceeded` so a runaway client
/// cannot tie up unbounded broadcast receivers + WS writer tasks on the
/// server. Matches the audio-streaming budget the dashboard exercises in
/// practice (≤4 tiles + headroom for transitions).
const MAX_STREAM_SUBS_PER_USER: usize = 8;

/// Per-user counter map. Each `acquire_slot` returns an RAII guard that
/// decrements the counter on drop; the guard lives inside the streaming
/// task so a client disconnect (which drops the task) releases the slot.
static STREAM_SUBS_PER_USER: OnceLock<DashMap<String, Arc<AtomicUsize>>> = OnceLock::new();

fn stream_subs_per_user() -> &'static DashMap<String, Arc<AtomicUsize>> {
    STREAM_SUBS_PER_USER.get_or_init(DashMap::new)
}

/// RAII guard owning one slot in the per-user counter. Increment happens at
/// construction (failure -> Err); decrement happens in `Drop`.
struct StreamSlotGuard {
    counter: Arc<AtomicUsize>,
}

impl StreamSlotGuard {
    fn acquire(user_id: &str) -> Result<Self, ProtocolError> {
        let counter = stream_subs_per_user()
            .entry(user_id.to_string())
            .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
            .clone();
        // Pre-increment-and-check: avoids the increment-then-decrement race
        // a load-then-store pattern would suffer under concurrent subscribes.
        let prev = counter.fetch_add(1, Ordering::AcqRel);
        if prev >= MAX_STREAM_SUBS_PER_USER {
            counter.fetch_sub(1, Ordering::AcqRel);
            return Err(ProtocolError::new(
                ProtocolErrorCode::RateLimited,
                format!(
                    "stream subscription limit reached ({} per user)",
                    MAX_STREAM_SUBS_PER_USER
                ),
            ));
        }
        Ok(Self { counter })
    }
}

impl Drop for StreamSlotGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

// -----------------------------------------------------------------------------
// Subscribe — streaming handler. Spawned by the dispatch layer with its own
// `Subscription` (mpsc) handle; the ws_binary writer drains the mpsc into the
// socket as IS_STREAM_CHUNK frames terminated by IS_STREAM_END.
// -----------------------------------------------------------------------------

fn stream_subscribe_handler(req: MessageBody, ctx: HandlerContext, sub: Arc<Subscription>) {
    let payload = match req {
        MessageBody::StreamBody(StreamPayload::SubscribeRequest(r)) => r,
        _ => {
            let _ = push_end(
                &sub,
                Some(MessageBody::Error(ProtocolError::bad_request(
                    "expected StreamSubscribeRequest variant",
                ))),
            );
            return;
        }
    };
    // `stream_id` is the PUBLIC id the client sent (and the one we echo back in
    // every frame). `hub_key` is the INTERNAL StreamHub key we subscribe under:
    // identical to `stream_id` for local cameras, but org/owner-scoped for
    // remote relays so two tenants (or two owner nodes) can never collide or
    // reuse the same `camera:<id>` source. The client never sees `hub_key`.
    let stream_id = payload.stream_id;

    // Per-stream-prefix permission gate. Camera streams require
    // `camera.read`; any other prefix is rejected pending a dedicated
    // permission wiring (no implicit allow — fail closed).
    let hub_key = match enforce_subscribe_permission(&ctx, &stream_id) {
        Ok(key) => key,
        Err(err) => {
            let _ = push_end(&sub, Some(MessageBody::Error(err)));
            return;
        }
    };

    // Per-user concurrency cap. Acquired before touching the hub so a
    // rejected request never instantiates a hub source. The guard moves
    // into the streaming task below and releases on task end / disconnect.
    let user_id = match user_id_from_ctx(&ctx) {
        Some(id) => id,
        None => {
            let _ = push_end(
                &sub,
                Some(MessageBody::Error(ProtocolError::new(
                    ProtocolErrorCode::AuthRequired,
                    "stream subscribe requires a user session",
                ))),
            );
            return;
        }
    };
    let slot_guard = match StreamSlotGuard::acquire(&user_id) {
        Ok(g) => g,
        Err(err) => {
            let _ = push_end(&sub, Some(MessageBody::Error(err)));
            return;
        }
    };

    tokio::spawn(async move {
        // Held for the lifetime of the streaming task — drop releases the
        // per-user slot back to the pool.
        let _slot = slot_guard;

        let handle = match StreamHub::global().subscribe(&hub_key).await {
            Ok(h) => h,
            Err(StreamHubError::NotRegistered(_)) => {
                // Echo the PUBLIC stream id, never the internal (org/owner
                // scoped) hub key, which would leak tenant/topology detail.
                let _ = push_end_async(
                    &sub,
                    Some(MessageBody::Error(ProtocolError::not_found(format!(
                        "stream_not_registered: {}",
                        stream_id
                    )))),
                )
                .await;
                return;
            }
            Err(err) => {
                let _ = push_end_async(
                    &sub,
                    Some(MessageBody::Error(ProtocolError::new(
                        ProtocolErrorCode::Internal,
                        format!("stream subscribe failed: {}", err),
                    ))),
                )
                .await;
                return;
            }
        };

        // 1) Subscribe ack — carries MIME + whether an init segment follows.
        let has_init_segment = handle.init_segment.is_some();
        if push_chunk_async(
            &sub,
            MessageBody::StreamBody(StreamPayload::SubscribeResponse(StreamSubscribeResponse {
                stream_id: stream_id.clone(),
                mime_type: handle.mime_type.clone(),
                has_init_segment,
            })),
        )
        .await
        .is_err()
        {
            return;
        }

        // 2) Init segment — emit before any media chunk. Frontend feeds this
        // straight into `SourceBuffer.appendBuffer` for MSE start-up.
        if let Some(init) = handle.init_segment.clone() {
            if push_chunk_async(
                &sub,
                MessageBody::StreamBody(StreamPayload::Frame(StreamFramePayload {
                    stream_id: stream_id.clone(),
                    is_init: true,
                    data: init.to_vec(),
                })),
            )
            .await
            .is_err()
            {
                return;
            }
        }

        // 3) Media chunks — drain the broadcast receiver. The `handle`
        // (and its inner SubscriptionHandle) lives on the stack so its
        // Drop runs on task exit, releasing the hub-side refcount.
        let mut receiver = handle.receiver;
        let final_reason = loop {
            match receiver.recv().await {
                Ok(chunk) => {
                    if push_chunk_async(
                        &sub,
                        MessageBody::StreamBody(StreamPayload::Frame(StreamFramePayload {
                            stream_id: stream_id.clone(),
                            is_init: false,
                            data: chunk.to_vec(),
                        })),
                    )
                    .await
                    .is_err()
                    {
                        // Writer detached — client gone; bail out without an
                        // explicit Closed frame (the writer side already
                        // sequenced an envelope-level End).
                        return;
                    }
                }
                Err(RecvError::Lagged(_)) => break "subscriber_lagged",
                Err(RecvError::Closed) => break "source_unregistered",
            }
        };

        // 4) Terminal Closed — single payload with a static reason tag.
        let _ = push_end_async(
            &sub,
            Some(MessageBody::StreamBody(StreamPayload::Closed(
                StreamClosedPayload {
                    stream_id,
                    reason: final_reason.to_string(),
                },
            ))),
        )
        .await;
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "StreamSubscribeRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: stream_subscribe_handler,
    }
}

// -----------------------------------------------------------------------------
// Close — synchronous handler. The client emits `StreamCloseRequest` on the
// SAME correlation id as the original subscribe; cancelling the registry
// subscription drops the broadcast receiver and trips the streaming task's
// Drop path, which releases the per-user slot and decrements the hub refcount.
// -----------------------------------------------------------------------------

#[handler(variant = "StreamCloseRequest", since = (2, 0))]
#[policy(UserSession)]
#[observed]
pub fn stream_close(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let registry = super::subscription::global();
    if registry.cancel(ctx.correlation_id) {
        // Returning a synthetic Closed echoes the contract: the caller
        // receives one terminal payload acknowledging the close. The streaming
        // task itself also emits a Closed via the cancel's Err event, but the
        // writer drops further frames after the first IS_STREAM_END, so the
        // client only ever sees one terminal envelope.
        Ok(MessageBody::StreamBody(StreamPayload::Closed(
            StreamClosedPayload {
                stream_id: String::new(),
                reason: "client_request".to_string(),
            },
        )))
    } else {
        Err(ProtocolError::not_found(
            "no active stream for this correlation_id",
        ))
    }
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn user_id_from_ctx(ctx: &HandlerContext) -> Option<String> {
    // Prefer the canonical membership user_id from `OrgContext` (matches the
    // audit-log key the rest of the dispatch layer uses). Fall back to a hex
    // form of the session user_id bytes if org context is unavailable — the
    // counter only needs a stable per-user discriminator.
    if let Some(org) = &ctx.org_context {
        return Some(org.user_id.clone());
    }
    if let SessionAuth::UserSession { user_id, .. } = &ctx.session {
        let mut s = String::with_capacity(32);
        for b in user_id.iter() {
            s.push_str(&format!("{:02x}", b));
        }
        return Some(s);
    }
    None
}

/// Audit a denied camera stream subscribe. Never echoes the camera_id (a denied
/// caller could be probing for ids in another tenant), only org + reason.
#[cfg(feature = "camera")]
fn audit_stream_denied(
    ctx: &HandlerContext,
    org: &crate::services::rbac::OrgContext,
    reason: &str,
) {
    let user_uuid = match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            Some(uuid::Uuid::from_bytes(*user_id).to_string())
        }
        _ => None,
    };
    let details = serde_json::json!({ "reason": reason }).to_string();
    let _ = crate::db::repository::log_audit_full(
        &ctx.state.db,
        user_uuid.as_deref(),
        None,
        "camera.stream_subscribe",
        Some("camera"),
        None,
        Some(&details),
        "info",
        "C",
        Some("denied"),
        Some(org.org_id.as_str()),
        None,
        Some(&ctx.state.local_node_id),
    );
}

/// Build the INTERNAL StreamHub key for a remote relay. Scoped by owner node +
/// org so a remote relay can never collide with a local `camera:<id>` source or
/// with another tenant's / another owner's relay for the same camera id. The
/// client never sees this key — it only ever sends/receives the public
/// `camera:<id>` stream id.
#[cfg(feature = "camera")]
fn remote_relay_hub_key(camera_id: &str, owner_node: &str, org_id: &str) -> String {
    format!("{}{}@{}#{}", CAMERA_PREFIX, camera_id, owner_node, org_id)
}

/// Resolve a trusted remote owner for `camera_id` (a camera not local to this
/// node) and, if found, idempotently register a `RemoteCameraStreamSource`
/// factory under the org/owner-scoped internal hub key. Returns `Some(hub_key)`
/// when a relay is available (owner resolved + mesh present), so the caller may
/// subscribe under that key; `None` means deny (unknown camera, no mesh,
/// cross-tenant).
///
/// The org-scope gate lives in `remote_camera_owner`: it only returns an owner
/// when the advertised robot's org equals the caller's org. The owner node
/// independently re-verifies this before serving (defence in depth).
#[cfg(feature = "camera")]
fn register_remote_camera_relay(
    ctx: &HandlerContext,
    org: &crate::services::rbac::OrgContext,
    camera_id: &str,
) -> Option<String> {
    let iroh = ctx.state.quic_mesh.as_ref()?;
    let owner = crate::dispatch::camera_admin::remote_camera_owner(
        &ctx.state.local_node_id,
        &org.org_id,
        camera_id,
    )?;

    // Register a factory under the scoped key; re-registering the same key is
    // idempotent (latest wins) and does not tear down an already-active relay.
    // The factory captures what it needs to open a fresh relay on the next cold
    // subscribe.
    let hub_key = remote_relay_hub_key(camera_id, &owner, &org.org_id);
    let iroh = Arc::clone(iroh);
    let owner = owner.clone();
    let camera_id = camera_id.to_string();
    let org_id = org.org_id.clone();
    let factory = Box::new(move || {
        let source = crate::services::camera_relay::source::RemoteCameraStreamSource::new(
            Arc::clone(&iroh),
            owner.clone(),
            camera_id.clone(),
            org_id.clone(),
        );
        Ok(source as Arc<dyn crate::services::stream_hub::BinaryStreamSource>)
    });
    let _ = StreamHub::global().register_factory(hub_key.clone(), factory);
    Some(hub_key)
}

/// Authorize a subscribe and resolve the INTERNAL StreamHub key to subscribe
/// under. For local cameras (and the no-camera build) this is the bare public
/// `stream_id`; for a remote relay it is the org/owner-scoped key returned by
/// `register_remote_camera_relay`.
fn enforce_subscribe_permission(
    ctx: &HandlerContext,
    stream_id: &str,
) -> Result<String, ProtocolError> {
    if let Some(rest) = stream_id.strip_prefix(CAMERA_PREFIX) {
        if rest.is_empty() {
            return Err(ProtocolError::bad_request("stream_id missing camera id"));
        }
        let org = ctx.org_context.as_ref().ok_or_else(|| {
            ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required")
        })?;
        if !org.has(PERM_CAMERA_READ) {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                "camera.read permission required",
            ));
        }
        // Org scoping: `camera.read` is not consent to read EVERY camera id. The
        // camera must belong to the caller's org, else a user could subscribe to
        // another tenant's stream by guessing its id. Cross-org / unknown id maps
        // to NotFound so existence in another tenant is never leaked.
        #[cfg(feature = "camera")]
        {
            return match crate::db::repository::camera_exists_in_org(
                &ctx.state.db,
                rest,
                &org.org_id,
            ) {
                // Local camera in this org: the hub key is the bare public id
                // (already org-checked via camera_exists_in_org).
                Ok(true) => Ok(stream_id.to_string()),
                // Not a local camera in this org. Before denying, check whether a
                // trusted mesh node in THIS org owns it (a robot camera physically
                // on another node). If so, lazily register a remote relay source
                // under the org/owner-scoped hub key and subscribe under THAT key.
                // Org scope is enforced here (remote_camera_owner requires the
                // advertised robot's org == caller org) AND on the owner side (it
                // re-verifies it advertises the camera in this org).
                Ok(false) => match register_remote_camera_relay(ctx, org, rest) {
                    Some(hub_key) => Ok(hub_key),
                    None => {
                        // Audit the cross-tenant probe; never echo the camera_id.
                        audit_stream_denied(ctx, org, "camera_not_in_org");
                        Err(ProtocolError::not_found(format!(
                            "stream_not_registered: {}{}",
                            CAMERA_PREFIX, rest
                        )))
                    }
                },
                Err(_) => Err(ProtocolError::new(
                    ProtocolErrorCode::Internal,
                    "camera org lookup failed",
                )),
            };
        }
        #[cfg(not(feature = "camera"))]
        return Ok(stream_id.to_string());
    }
    Err(ProtocolError::new(
        ProtocolErrorCode::PolicyDenied,
        format!("unsupported stream prefix: {}", stream_id),
    ))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::subscription::{
        find_stream_handler, SubscriptionEvent, SubscriptionRegistry,
    };
    use crate::dispatch::HandlerContext;
    use crate::services::stream_hub::{BinaryStreamSource, StreamHub};
    use async_trait::async_trait;
    use bytes::Bytes;
    use tentaflow_protocol::{StreamPayload, StreamSubscribeRequest};
    use tokio::sync::broadcast;

    struct StubSource {
        id: String,
        mime: String,
        init: Option<Bytes>,
        tx: broadcast::Sender<Bytes>,
    }

    #[async_trait]
    impl BinaryStreamSource for StubSource {
        fn id(&self) -> &str {
            &self.id
        }
        fn mime_type(&self) -> &str {
            &self.mime
        }
        async fn init_segment(&self) -> Option<Bytes> {
            self.init.clone()
        }
        fn chunk_broadcaster(&self) -> Option<broadcast::Sender<Bytes>> {
            Some(self.tx.clone())
        }
    }

    fn register_stub_source(
        stream_id: &'static str,
        init: Option<Bytes>,
    ) -> broadcast::Sender<Bytes> {
        let (tx, _rx) = broadcast::channel(8);
        let tx_clone = tx.clone();
        let id = stream_id.to_string();
        let mime = "video/mp4; codecs=\"avc1.64001f\"".to_string();
        let init_for_factory = init.clone();
        StreamHub::global()
            .register_factory(
                stream_id.to_string(),
                Box::new(move || {
                    let src: Arc<dyn BinaryStreamSource> = Arc::new(StubSource {
                        id: id.clone(),
                        mime: mime.clone(),
                        init: init_for_factory.clone(),
                        tx: tx_clone.clone(),
                    });
                    Ok(src)
                }),
            )
            .expect("register factory");
        tx
    }

    fn ctx_with_camera_read(correlation_id: u64) -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [1u8; 16],
                role: Some("admin".to_string()),
            },
            correlation_id,
            connection_id: 0,
            resume_secret: None,
            state: crate::dispatch::state::AppState::for_test(),
            org_context: Some(test_org_context("user-stream-test", PERM_CAMERA_READ)),
        }
    }

    /// Seed a local camera row in `ctx`'s DB under the ctx's org so the local
    /// `camera_exists_in_org` gate passes and the subscribe takes the LOCAL hub
    /// key path (bare `camera:<id>`), matching what these tests exercise.
    #[cfg(feature = "camera")]
    fn seed_local_camera(ctx: &HandlerContext, camera_id: &str) {
        let org_id = ctx.org_context.as_ref().unwrap().org_id.clone();
        crate::db::repository::insert_camera(
            &ctx.state.db,
            camera_id,
            "test-owner",
            "Cam",
            "webrtc",
            "",
            30,
            10,
            None,
            None,
            "C",
            "default",
            None,
            None,
            None,
            Some(&org_id),
        )
        .expect("seed local camera");
    }

    #[tokio::test]
    async fn subscribe_emits_response_then_init_then_frame_then_end() {
        let stream_id = "camera:test-cam-emit";
        let init = Bytes::from_static(&[0xAA, 0xBB]);
        let tx = register_stub_source(stream_id, Some(init.clone()));

        let reg = SubscriptionRegistry::new();
        let (sub, mut rx) = reg.create(101, None);
        let req =
            MessageBody::StreamBody(StreamPayload::SubscribeRequest(StreamSubscribeRequest {
                stream_id: stream_id.to_string(),
            }));
        let h = find_stream_handler("StreamSubscribeRequest").expect("registered");
        let ctx = ctx_with_camera_read(101);
        seed_local_camera(&ctx, "test-cam-emit");
        (h.handler_fn)(req, ctx, sub);

        // 1) SubscribeResponse.
        match rx.recv().await.unwrap() {
            SubscriptionEvent::Chunk(MessageBody::StreamBody(
                StreamPayload::SubscribeResponse(resp),
            )) => {
                assert_eq!(resp.stream_id, stream_id);
                assert!(resp.has_init_segment);
                assert!(resp.mime_type.starts_with("video/mp4"));
            }
            other => panic!("expected SubscribeResponse, got {:?}", other),
        }

        // 2) Init frame.
        match rx.recv().await.unwrap() {
            SubscriptionEvent::Chunk(MessageBody::StreamBody(StreamPayload::Frame(f))) => {
                assert!(f.is_init);
                assert_eq!(f.data, vec![0xAA, 0xBB]);
            }
            other => panic!("expected init Frame, got {:?}", other),
        }

        // 3) Push one media chunk; expect a non-init Frame.
        let _ = tx.send(Bytes::from_static(&[0x01, 0x02, 0x03]));
        match rx.recv().await.unwrap() {
            SubscriptionEvent::Chunk(MessageBody::StreamBody(StreamPayload::Frame(f))) => {
                assert!(!f.is_init);
                assert_eq!(f.data, vec![0x01, 0x02, 0x03]);
            }
            other => panic!("expected media Frame, got {:?}", other),
        }

        // The streaming task remains alive as long as the broadcast source
        // (held by the stub) has a sender clone. End-of-stream paths are
        // exercised in dedicated tests (`lagged_subscriber_emits_closed`).
    }

    #[tokio::test]
    async fn lagged_subscriber_emits_closed() {
        // Override the small broadcast cap by NOT draining the streaming task's
        // mpsc; the streaming task forwards via `push_chunk_async` which
        // back-pressures on a full mpsc, but the broadcast receiver eventually
        // lags when the source produces more frames than the broadcast cap
        // (8 in our stub) and the receiver is stuck on `push_chunk_async`.
        let stream_id = "camera:test-cam-lag";
        let tx = register_stub_source(stream_id, None);

        let reg = SubscriptionRegistry::new();
        let (sub, mut rx) = reg.create(606, None);
        let req =
            MessageBody::StreamBody(StreamPayload::SubscribeRequest(StreamSubscribeRequest {
                stream_id: stream_id.to_string(),
            }));
        let mut ctx = ctx_with_camera_read(606);
        ctx.org_context = Some(test_org_context("lag-user", PERM_CAMERA_READ));
        seed_local_camera(&ctx, "test-cam-lag");
        let h = find_stream_handler("StreamSubscribeRequest").unwrap();
        (h.handler_fn)(req, ctx, sub);

        // Drain the SubscribeResponse so the streaming task moves on to the
        // recv-loop. Then publish far more than the broadcast cap (8) before
        // we drain further events; the receiver must observe `Lagged`.
        match rx.recv().await.unwrap() {
            SubscriptionEvent::Chunk(MessageBody::StreamBody(
                StreamPayload::SubscribeResponse(_),
            )) => {}
            other => panic!("expected SubscribeResponse, got {:?}", other),
        }

        // Flood far beyond the broadcast cap before the streaming task has a
        // chance to drain. The mpsc cap (256) easily absorbs the burst; what
        // we are forcing is the BROADCAST receiver inside the streaming task
        // to fall behind, which surfaces as `RecvError::Lagged`.
        for i in 0..1024u32 {
            let _ = tx.send(Bytes::from(i.to_le_bytes().to_vec()));
        }

        // Drain whatever the task forwarded plus the terminal Closed(lagged).
        let mut got_terminal = false;
        for _ in 0..2048 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
                Ok(Some(SubscriptionEvent::Chunk(_))) => continue,
                Ok(Some(SubscriptionEvent::End(Some(MessageBody::StreamBody(
                    StreamPayload::Closed(c),
                ))))) => {
                    assert_eq!(c.reason, "subscriber_lagged");
                    assert_eq!(c.stream_id, stream_id);
                    got_terminal = true;
                    break;
                }
                Ok(other) => panic!("unexpected event: {:?}", other),
                Err(_) => break,
            }
        }
        assert!(got_terminal, "stream did not terminate with Closed(lagged)");
    }

    #[tokio::test]
    async fn subscribe_rejects_unregistered_stream() {
        let reg = SubscriptionRegistry::new();
        let (sub, mut rx) = reg.create(202, None);
        let req =
            MessageBody::StreamBody(StreamPayload::SubscribeRequest(StreamSubscribeRequest {
                stream_id: "camera:does-not-exist-xyz".to_string(),
            }));
        let h = find_stream_handler("StreamSubscribeRequest").unwrap();
        (h.handler_fn)(req, ctx_with_camera_read(202), sub);

        match rx.recv().await.unwrap() {
            SubscriptionEvent::End(Some(MessageBody::Error(e))) => {
                assert_eq!(e.code, ProtocolErrorCode::NotFound);
                assert!(e.message.contains("stream_not_registered"));
            }
            other => panic!("expected End(Error NotFound), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn subscribe_rejects_unsupported_prefix() {
        let reg = SubscriptionRegistry::new();
        let (sub, mut rx) = reg.create(303, None);
        let req =
            MessageBody::StreamBody(StreamPayload::SubscribeRequest(StreamSubscribeRequest {
                stream_id: "audio:doorbell".to_string(),
            }));
        let h = find_stream_handler("StreamSubscribeRequest").unwrap();
        (h.handler_fn)(req, ctx_with_camera_read(303), sub);

        match rx.recv().await.unwrap() {
            SubscriptionEvent::End(Some(MessageBody::Error(e))) => {
                assert_eq!(e.code, ProtocolErrorCode::PolicyDenied);
                assert!(e.message.contains("unsupported stream prefix"));
            }
            other => panic!("expected End(Error PolicyDenied), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn subscribe_rejects_missing_camera_read() {
        let stream_id = "camera:perm-test-cam";
        let _tx = register_stub_source(stream_id, None);

        let reg = SubscriptionRegistry::new();
        let (sub, mut rx) = reg.create(404, None);
        let req =
            MessageBody::StreamBody(StreamPayload::SubscribeRequest(StreamSubscribeRequest {
                stream_id: stream_id.to_string(),
            }));
        let mut ctx = ctx_with_camera_read(404);
        // Strip the permission to exercise the deny path.
        ctx.org_context = Some(test_org_context("user-no-perm", "some.other.perm"));
        let h = find_stream_handler("StreamSubscribeRequest").unwrap();
        (h.handler_fn)(req, ctx, sub);

        match rx.recv().await.unwrap() {
            SubscriptionEvent::End(Some(MessageBody::Error(e))) => {
                assert_eq!(e.code, ProtocolErrorCode::PolicyDenied);
                assert!(e.message.contains("camera.read"));
            }
            other => panic!("expected End(Error PolicyDenied), got {:?}", other),
        }
    }

    fn test_org_context(user_id: &str, perm: &str) -> crate::services::rbac::OrgContext {
        let mut permissions = std::collections::HashSet::new();
        permissions.insert(perm.to_string());
        crate::services::rbac::OrgContext {
            user_id: user_id.to_string(),
            org_id: "org-1".to_string(),
            role_id: "role-1".to_string(),
            permissions,
        }
    }

    #[cfg(feature = "camera")]
    #[test]
    fn remote_relay_hub_key_is_owner_and_org_scoped() {
        // The internal hub key for a remote relay must encode camera id + owner
        // node + org so it can never collide with a local `camera:<id>` source
        // or with another tenant's / owner's relay for the same camera id.
        let key = remote_relay_hub_key("cam-x", "node-A", "org-1");
        assert_eq!(key, "camera:cam-x@node-A#org-1");
        // Distinct owner or org yields a distinct key.
        assert_ne!(key, remote_relay_hub_key("cam-x", "node-B", "org-1"));
        assert_ne!(key, remote_relay_hub_key("cam-x", "node-A", "org-2"));
        // The bare local key (what a local camera uses) never collides.
        assert_ne!(key, format!("{}{}", CAMERA_PREFIX, "cam-x"));
    }

    #[tokio::test]
    async fn per_user_subscription_cap_enforced() {
        let stream_id = "camera:cap-test-cam";
        let _tx = register_stub_source(stream_id, None);

        // Build N+1 fresh subscriptions for the SAME user_id; the (N+1)th
        // subscribe must trip the QuotaExceeded path.
        let user_key = "cap-user";
        let mut handles = Vec::new();
        for i in 0..MAX_STREAM_SUBS_PER_USER {
            let reg = SubscriptionRegistry::new();
            let (sub, rx) = reg.create(900 + i as u64, None);
            let mut ctx = ctx_with_camera_read(900 + i as u64);
            ctx.org_context = Some(test_org_context(user_key, PERM_CAMERA_READ));
            seed_local_camera(&ctx, "cap-test-cam");
            let req =
                MessageBody::StreamBody(StreamPayload::SubscribeRequest(StreamSubscribeRequest {
                    stream_id: stream_id.to_string(),
                }));
            let h = find_stream_handler("StreamSubscribeRequest").unwrap();
            (h.handler_fn)(req, ctx, sub);
            handles.push(rx);
        }

        // Drain the SubscribeResponse from each of the N accepted subs to
        // ensure the streaming task has fully spawned and incremented the
        // slot counter before we attempt the overflowing (N+1)th subscribe.
        for rx in handles.iter_mut() {
            let _ = rx.recv().await.expect("ack");
        }

        // (N+1)th subscribe must be denied with QuotaExceeded.
        let reg = SubscriptionRegistry::new();
        let (sub, mut rx_over) = reg.create(999, None);
        let mut ctx = ctx_with_camera_read(999);
        ctx.org_context = Some(test_org_context(user_key, PERM_CAMERA_READ));
        seed_local_camera(&ctx, "cap-test-cam");
        let req =
            MessageBody::StreamBody(StreamPayload::SubscribeRequest(StreamSubscribeRequest {
                stream_id: stream_id.to_string(),
            }));
        let h = find_stream_handler("StreamSubscribeRequest").unwrap();
        (h.handler_fn)(req, ctx, sub);

        match rx_over.recv().await.unwrap() {
            SubscriptionEvent::End(Some(MessageBody::Error(e))) => {
                assert_eq!(e.code, ProtocolErrorCode::RateLimited);
                assert!(e.message.contains("limit reached"));
            }
            other => panic!("expected End(Error RateLimited), got {:?}", other),
        }
    }
}
