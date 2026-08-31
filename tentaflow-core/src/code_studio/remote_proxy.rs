// ===== File: code_studio/remote_proxy.rs — a workspace runs on its owner node, and only there =====
//
// §3 of the plan is blunt about the split: the node showing the dashboard has a
// proxy and NO decision logic. It serializes the request, mints a
// `SessionAssertion` for the person making it, sends it over the mesh, and
// hands back whatever the owner node answered. It does not consult the PEP, it
// does not open a workspace database, it does not decide anything.
//
// The owner node does the opposite: it verifies the assertion (identity only),
// then authorizes FROM SCRATCH — permission matrix, org membership, workspace
// role, PEP, containment — and executes through the SAME handler a local caller
// would reach. There is exactly one implementation of Code Studio behaviour;
// this module is transport.
//
// Two things this module deliberately does not do:
//
//   * it never writes an `unreachable` status anywhere. Whether the owner node
//     answers is a property of the network at this instant, not of the
//     workspace, so it surfaces as a protocol error the UI can project as
//     connectivity (§3.5);
//   * it never lets a revocation ride out the assertion lifetime for an
//     IRREVERSIBLE operation. A push, a merge or a credential change costs one
//     extra round trip to the issuing node, which re-resolves the actor's
//     permission against its live database before the owner node acts (§12.1).

use std::sync::Arc;

use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType, SessionAssertion};
use tentaflow_protocol::{
    code_studio::CodeStudioPayload, MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth,
};

use super::assertion::{self, AssertionError};
use super::models::WorkspaceRecord;
use super::operations::{op_id, OriginKind};
use super::pep::Capability;
use super::repository;
use crate::dispatch::{AppState, HandlerContext};
use crate::mesh::iroh_manager::IrohMeshManager;

/// How long the caller waits for the owner node to run one operation. Longer
/// than the assertion lifetime on purpose: the assertion is checked when the
/// request ARRIVES, and a git operation may legitimately run past that.
const OP_TIMEOUT_SECS: u64 = 45;

/// The freshness probe and the key pull are both single lookups on the far
/// side; they get a short deadline so a slow peer cannot stall an operation.
const PROBE_TIMEOUT_SECS: u64 = 5;

// =============================================================================
// Owner-side context
// =============================================================================

static OWNER_STATE: std::sync::OnceLock<Arc<AppState>> = std::sync::OnceLock::new();

/// How often the assertion signing key is replaced. The retired key stays
/// verifiable for `assertion::KEY_OVERLAP_MS` afterwards, so a rotation is
/// invisible to sessions that are running.
const KEY_ROTATION_INTERVAL: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);

/// Give the mesh receive path the shared server resources it needs to execute a
/// forwarded request through the ordinary handler, and start the assertion key
/// lifecycle. Called once at startup; the first registration wins, mirroring
/// `mesh::robot_dispatch::set_dispatch_context`.
pub fn install_owner_context(state: Arc<AppState>) -> bool {
    if OWNER_STATE.set(state).is_err() {
        return false;
    }
    // The key lifecycle only makes sense with a runtime and a mesh; a
    // single-node deployment simply never advertises anything.
    if let (Ok(handle), Some(state)) = (tokio::runtime::Handle::try_current(), OWNER_STATE.get()) {
        let state = state.clone();
        handle.spawn(key_maintenance(state));
    }
    true
}

/// Advertise the local assertion keys to trust-paired peers, then rotate on a
/// fixed cadence and advertise again. A peer that misses an advertisement pulls
/// on the first unknown `kid` it meets, so this is the fast path, not the only
/// one.
async fn key_maintenance(state: Arc<AppState>) {
    announce_assertion_keys(&state).await;
    loop {
        tokio::time::sleep(KEY_ROTATION_INTERVAL).await;
        assertion::rotate_local_key();
        announce_assertion_keys(&state).await;
    }
}

async fn announce_assertion_keys(state: &Arc<AppState>) {
    let (Some(iroh), Some(security)) = (state.quic_mesh.as_ref(), state.mesh_security.as_ref())
    else {
        return;
    };
    let peers = broadcast_assertion_keys(iroh, security).await;
    tracing::debug!(peers, "code studio: assertion keys advertised");
}

/// The shared server resources this node registered at startup.
///
/// `install_owner_context` is the only writer, so this is `None` until the
/// server has booted (and in a unit test that never boots one). Callers outside
/// the mesh receive path use it for what only the process as a whole can own —
/// the model router behind the semantic index, for instance — and must treat
/// `None` as "this capability is not wired on this node", never as an error.
pub fn node_state() -> Option<&'static Arc<AppState>> {
    OWNER_STATE.get()
}

// =============================================================================
// Operation identity and the irreversible set
// =============================================================================

/// Identity of one forwarded call, computed identically on both nodes.
///
/// It is `operations::op_id` with the UI origin and the payload digest as the
/// origin id, so the same request re-sent after a timeout yields the same id —
/// which is precisely what makes a replay harmless: the operation journal
/// deduplicates by `op_id` (§13.1). The owner node recomputes it from the bytes
/// it received and refuses an assertion that claims a different one.
pub fn remote_op_id(session_id: &str, payload_cbor: &[u8]) -> String {
    op_id(
        session_id,
        OriginKind::Ui,
        &assertion::digest_hex(payload_cbor),
        MESH_LOGICAL_STEP,
    )
}

/// Logical step every forwarded call journals under. One mesh request is one
/// operation, so the step names the transport rather than the verb — that is
/// what lets the owner node derive the SAME id the assertion was bound to.
pub const MESH_LOGICAL_STEP: &str = "mesh_op";

tokio::task_local! {
    /// Payload digest of the forwarded request the current task is serving.
    static REMOTE_ORIGIN: String;
}

/// Origin id a forwarded call must be journaled under, or `None` when the
/// request arrived on this node's own socket.
///
/// The operation journal deduplicates by `op_id` (§13.1), and the assertion
/// binds one. They have to be the same value or a retried mesh request would
/// open a second row and run the effect twice, so the owner-side handler asks
/// here instead of deriving an origin from a connection that does not exist.
pub fn current_remote_origin_id() -> Option<String> {
    REMOTE_ORIGIN.try_with(|origin| origin.clone()).ok()
}

/// Runs `future` as if it were serving a call forwarded over the mesh, with
/// `origin` as the payload digest the operation journal keys on.
pub async fn with_remote_origin<F, T>(origin: String, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    REMOTE_ORIGIN.scope(origin, future).await
}

/// The capability of an operation whose effect cannot be taken back, or `None`.
///
/// Only the four `Capability::is_mandatory_interactive` operations are listed —
/// that method is the single definition of the set, and this match is its
/// wire-level projection. Every other request is authorized entirely by the
/// owner-side handler; nothing here decides anything about them.
fn irreversible_capability(payload: &CodeStudioPayload) -> Option<Capability> {
    let capability = match payload {
        CodeStudioPayload::GitPushRequest { .. } => Capability::GitPush,
        CodeStudioPayload::GitMergeRequest { .. } => Capability::GitMerge,
        CodeStudioPayload::GitMergeFinalizeRequest { .. } => Capability::GitMergeFinalize,
        CodeStudioPayload::WorkspaceSecretSetRequest { .. } => Capability::SecretManage,
        _ => return None,
    };
    debug_assert!(capability.is_mandatory_interactive());
    Some(capability)
}

fn encode(payload: &CodeStudioPayload) -> Result<Vec<u8>, ProtocolError> {
    crate::mesh::cbor::encode(payload)
        .map_err(|e| ProtocolError::internal(format!("code studio payload encode failed: {e}")))
}

fn decode(bytes: &[u8]) -> Result<CodeStudioPayload, ProtocolError> {
    crate::mesh::cbor::decode(bytes)
        .map_err(|e| ProtocolError::bad_request(format!("code studio payload decode failed: {e}")))
}

fn unreachable(node_id: &str, detail: impl std::fmt::Display) -> ProtocolError {
    // Connectivity, not state: nothing about this is persisted (§3.5).
    ProtocolError::new(
        ProtocolErrorCode::NodeUnreachable,
        format!("owner node '{node_id}' did not answer: {detail}"),
    )
}

fn assertion_denied(error: &AssertionError) -> ProtocolError {
    let code = match error {
        AssertionError::Db(_) => ProtocolErrorCode::Internal,
        AssertionError::Identity(_) => ProtocolErrorCode::AuthRequired,
        _ => ProtocolErrorCode::PolicyDenied,
    };
    ProtocolError::new(code, error.to_string())
}

// =============================================================================
// Dashboard side — the proxy
// =============================================================================

/// Forward one Code Studio request to the node that owns the workspace.
///
/// The caller has already established that the workspace is remote; everything
/// authorization-shaped happens on the far side.
pub async fn proxy_to_owner(
    ctx: &HandlerContext,
    record: &WorkspaceRecord,
    payload: &CodeStudioPayload,
    session_id: &str,
) -> Result<MessageBody, ProtocolError> {
    forward(ctx, &record.node_id, &record.id, session_id, payload).await
}

/// Workspace an assertion names when the call CREATES one.
///
/// `verify_claims` requires the field to be present, and at this moment there
/// is nothing to name: the id is minted by the node that does the work. The
/// sentinel says so explicitly rather than smuggling an empty string past a
/// check that exists to catch exactly that. It grants nothing — the owner node
/// re-resolves the caller's org membership and creation grant locally, and a
/// role lookup for this id finds no membership by construction.
pub const WORKSPACE_PENDING: &str = "pending-create";

/// Forward one call to a node addressed DIRECTLY, for the request that has no
/// workspace row to be addressed by: creating one.
///
/// Provisioning runs where the workspace will live — the directory, the git
/// repository, the runtime database and the authoritative `egress_enforcement`
/// are all facts of that node (§7.6) — so the wizard's request travels and the
/// secret material lands in the target node's vault, encrypted with ITS key
/// (§5.2), instead of resting on the node somebody happened to click on.
pub async fn proxy_to_node(
    ctx: &HandlerContext,
    node_id: &str,
    payload: &CodeStudioPayload,
) -> Result<MessageBody, ProtocolError> {
    forward(ctx, node_id, WORKSPACE_PENDING, "", payload).await
}

async fn forward(
    ctx: &HandlerContext,
    node_id: &str,
    workspace_id: &str,
    session_id: &str,
    payload: &CodeStudioPayload,
) -> Result<MessageBody, ProtocolError> {
    let org = ctx.org_context.as_ref().ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required")
    })?;
    let iroh = ctx.state.quic_mesh.as_ref().cloned().ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "mesh transport is not running on this node",
        )
    })?;
    if !iroh.is_trusted(node_id) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!("node '{node_id}' is not a trusted peer"),
        ));
    }

    let payload_cbor = encode(payload)?;
    let caps = assertion::resolve_caps(&ctx.state.db, &org.user_id, workspace_id)
        .map_err(|e| assertion_denied(&e))?;
    let rbac_rev = assertion::rbac_revision(&ctx.state.db, &org.user_id, &org.org_id, workspace_id)
        .map_err(|e| assertion_denied(&e))?;
    let op = remote_op_id(session_id, &payload_cbor);
    let minted = assertion::issue(&assertion::IssueRequest {
        local_node_id: &ctx.state.local_node_id,
        user_id: &org.user_id,
        owner_node_id: node_id,
        org_id: &org.org_id,
        workspace_id,
        session_id,
        caps: &caps,
        rbac_rev: &rbac_rev,
        op_id: &op,
        payload_cbor: &payload_cbor,
        lifetime_ms: assertion::DEFAULT_LIFETIME_MS,
    })
    .map_err(|e| assertion_denied(&e))?;

    let response = iroh
        .send_command_and_wait(
            node_id,
            MeshCommandType::CodeStudioOp {
                assertion: minted,
                payload_cbor,
            },
            OP_TIMEOUT_SECS,
        )
        .await
        .map_err(|e| unreachable(node_id, e))?;

    let (payload, transport_error) = (response.payload, response.error);
    match payload {
        MeshCommandResponsePayload::CodeStudioOpResult {
            payload_cbor,
            error,
        } => match error {
            Some(error) => Err(error),
            None => Ok(MessageBody::CodeStudioBody(decode(&payload_cbor)?)),
        },
        _ => Err(unreachable(
            node_id,
            transport_error.unwrap_or_else(|| "unexpected mesh response payload".to_string()),
        )),
    }
}

// =============================================================================
// Streams (§12.2) — minting on the consumer side, authorizing on the owner side
// =============================================================================

/// Uniform refusal for every stream call an actor may not make.
///
/// A member who is not the session's owner, a session that does not exist and a
/// workspace this node does not run all produce THIS text, so a member cannot
/// use the difference between two refusals to find out whose sessions exist
/// (§5.3, §25.4). It is deliberately a `&'static str` rather than a formatted
/// message: a formatted one grows an id sooner or later.
pub const STREAM_NOT_FOUND: &str = "code studio session not found";

fn stream_not_found() -> ProtocolError {
    ProtocolError::new(ProtocolErrorCode::NotFound, STREAM_NOT_FOUND)
}

/// Mint the assertion that authorizes ONE stream call.
///
/// A stream lives for hours and an assertion for at most 120 s, so the consumer
/// mints per call rather than once: that is what makes the owner node's
/// authorization current instead of a snapshot taken when the browser
/// subscribed. `op_id` is empty because reading a stream journals no operation
/// — `args_digest` is what binds the call to its parameters here.
pub fn mint_stream_assertion(
    db: &crate::db::DbPool,
    local_node_id: &str,
    owner_node_id: &str,
    user_id: &str,
    org_id: &str,
    workspace_id: &str,
    session_id: &str,
    request_cbor: &[u8],
) -> Result<SessionAssertion, ProtocolError> {
    let caps =
        assertion::resolve_caps(db, user_id, workspace_id).map_err(|e| assertion_denied(&e))?;
    let rbac_rev = assertion::rbac_revision(db, user_id, org_id, workspace_id)
        .map_err(|e| assertion_denied(&e))?;
    assertion::issue(&assertion::IssueRequest {
        local_node_id,
        user_id,
        owner_node_id,
        org_id,
        workspace_id,
        session_id,
        caps: &caps,
        rbac_rev: &rbac_rev,
        op_id: "",
        payload_cbor: request_cbor,
        lifetime_ms: assertion::DEFAULT_LIFETIME_MS,
    })
    .map_err(|e| assertion_denied(&e))
}

/// Owner-side check every stream command starts with: the same verification
/// `CodeStudioOp` runs, plus the rule that a stream credential carries no
/// operation. An assertion minted for a mutating call must not double as a
/// ticket to read somebody's terminal, and vice versa.
async fn verify_stream_assertion(
    from_node_id: &str,
    assertion_in: &SessionAssertion,
    request_cbor: &[u8],
    iroh: &Arc<IrohMeshManager>,
) -> Result<(Arc<AppState>, assertion::VerifiedAssertion), ProtocolError> {
    let state = node_state().cloned().ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "code studio mesh context is not initialized on this node",
        )
    })?;
    let verify_input = assertion::VerifyInput {
        channel_peer_id: from_node_id,
        local_node_id: &state.local_node_id,
        payload_cbor: request_cbor,
        now_unix_ms: crate::services::mesh_keys::now_unix_ms(),
    };
    if let Err(AssertionError::UnknownKey { .. }) =
        assertion::verify_claims(assertion_in, &verify_input)
    {
        fetch_peer_assertion_keys(from_node_id, iroh).await;
    }
    let verified = assertion::verify(&state.db, assertion_in, &verify_input)
        .map_err(|e| assertion_denied(&e))?;
    if !verified.op_id.is_empty() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "an operation assertion does not authorize a stream",
        ));
    }
    Ok((state, verified))
}

/// Serve `CodeStudioStreamOpen`: authorize the ACTOR named by the assertion,
/// then start producing for them.
///
/// Returns the producer's highest sequence, or the refusal to send back. The
/// refusal is the uniform one whenever it could otherwise reveal that somebody
/// else's session exists.
pub async fn open_owner_stream(
    from_node_id: &str,
    assertion_in: &SessionAssertion,
    request_cbor: &[u8],
    iroh: &Arc<IrohMeshManager>,
) -> Result<u64, ProtocolError> {
    let (state, verified) =
        verify_stream_assertion(from_node_id, assertion_in, request_cbor, iroh).await?;
    let request: tentaflow_protocol::mesh::CodeStudioStreamOpenRequest =
        crate::mesh::cbor::decode(request_cbor).map_err(|e| {
            ProtocolError::bad_request(format!("stream open request decode failed: {e}"))
        })?;
    // The assertion names the workspace and the session it was minted for; the
    // request must not widen either.
    if request.workspace_id != verified.workspace_id || request.session_id != verified.session_id {
        return Err(stream_not_found());
    }
    crate::dispatch::stream_handlers::code_studio_open_owner_stream(
        &state,
        &verified,
        from_node_id,
        &request,
    )
    .await
    .map_err(stream_refusal)
}

/// Serve `CodeStudioStreamPull`.
///
/// Three things are re-established on EVERY read, none of them from anything
/// cached when the stream opened: the assertion (so the actor is current and
/// the call is not a replay), the org role and workspace membership from this
/// node's tables, and the binding of the stream to `(peer node, actor)`. What
/// is NOT repeated per read is the session-ownership lookup in the runtime
/// database: a session's owner is immutable, it was checked when the stream
/// opened, and the producer re-checks the whole gate every
/// `CS_REVALIDATE_EVERY` — paying a second database for it twenty times a
/// second would buy nothing.
///
/// A stream whose actor lost access is closed here with a stated reason and the
/// close record travels back on this very read, so the browser learns why its
/// terminal stopped instead of watching it fall silent.
pub async fn pull_owner_stream(
    from_node_id: &str,
    assertion_in: &SessionAssertion,
    request_cbor: &[u8],
    iroh: &Arc<IrohMeshManager>,
) -> Result<super::mesh_stream::PullResult, ProtocolError> {
    let (state, verified) =
        verify_stream_assertion(from_node_id, assertion_in, request_cbor, iroh).await?;
    let request: tentaflow_protocol::mesh::CodeStudioStreamPullRequest =
        crate::mesh::cbor::decode(request_cbor).map_err(|e| {
            ProtocolError::bad_request(format!("stream pull request decode failed: {e}"))
        })?;
    if request.session_id != verified.session_id {
        return Err(stream_not_found());
    }

    let (db, node_id) = (state.db.clone(), state.local_node_id.to_string());
    let (user_id, org_id, workspace_id) = (
        verified.user_id.clone(),
        verified.org_id.clone(),
        verified.workspace_id.clone(),
    );
    let allowed = tokio::task::spawn_blocking(move || {
        crate::dispatch::stream_handlers::code_studio_authorize_stream_read(
            &db,
            &node_id,
            &user_id,
            &org_id,
            &workspace_id,
        )
        .map(|_| ())
    })
    .await
    .map_err(|e| ProtocolError::internal(format!("stream authorization task failed: {e}")))?;

    if let Err(reason) = allowed {
        const DETAIL: &str = "the actor's access to this workspace ended";
        // This actor's stream is closed with the reason — only theirs, because
        // a refusal must not reach anybody else's session — and the close
        // record is handed back on this very read. A stream ends by SAYING it
        // ended (§12.2); answering with a transport failure instead would make
        // a revoked role look like a lost node.
        super::mesh_stream::hub().close_for_peer(
            from_node_id,
            &verified.user_id,
            &request.session_id,
            &request.stream_id,
            reason,
            DETAIL,
        );
        return Ok(super::mesh_stream::PullResult {
            frames: Vec::new(),
            close: Some(tentaflow_protocol::mesh::CodeStudioStreamClose {
                reason: reason.to_string(),
                detail: DETAIL.to_string(),
            }),
            highest_seq: 0,
        });
    }

    super::mesh_stream::hub()
        .pull_for_peer(
            from_node_id,
            &verified.user_id,
            &request.session_id,
            &request.stream_id,
            request.after_seq,
            request.ack_seq,
            request.credits,
        )
        .map_err(|_| stream_not_found())
}

/// Turns a stream gate's reason into the answer that travels back.
///
/// Everything that could betray the existence of another person's session
/// collapses into ONE refusal; a refusal about the caller's OWN standing
/// (`permission_revoked`) keeps its own code, because it reveals nothing the
/// caller does not already know about themselves.
fn stream_refusal(reason: &str) -> ProtocolError {
    match reason {
        crate::dispatch::stream_handlers::CS_END_PERMISSION_REVOKED => {
            ProtocolError::new(ProtocolErrorCode::PolicyDenied, reason)
        }
        crate::dispatch::stream_handlers::CS_END_INTERNAL => {
            ProtocolError::internal("code studio stream could not be opened")
        }
        _ => stream_not_found(),
    }
}

// =============================================================================
// Owner side — verification, freshness, execution
// =============================================================================

/// Run a forwarded request on the node that owns the workspace.
///
/// Returns the encoded response payload, or the protocol error the local
/// handler produced — the code survives the trip, so the calling node shows the
/// same answer a local caller would have seen.
pub async fn execute_owner_side(
    from_node_id: &str,
    assertion_in: &SessionAssertion,
    payload_cbor: &[u8],
    iroh: &Arc<IrohMeshManager>,
) -> (Vec<u8>, Option<ProtocolError>) {
    match execute_owner_side_inner(from_node_id, assertion_in, payload_cbor, iroh).await {
        Ok(bytes) => (bytes, None),
        Err(error) => (Vec::new(), Some(error)),
    }
}

async fn execute_owner_side_inner(
    from_node_id: &str,
    assertion_in: &SessionAssertion,
    payload_cbor: &[u8],
    iroh: &Arc<IrohMeshManager>,
) -> Result<Vec<u8>, ProtocolError> {
    let state = node_state().cloned().ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "code studio mesh context is not initialized on this node",
        )
    })?;

    let verify_input = assertion::VerifyInput {
        channel_peer_id: from_node_id,
        local_node_id: &state.local_node_id,
        payload_cbor,
        now_unix_ms: crate::services::mesh_keys::now_unix_ms(),
    };

    // A key we have never seen is the ordinary case right after a peer restarts
    // or rotates, so pull the issuer's ring once before giving up. The pull goes
    // to the SAME authenticated peer the assertion arrived from — a node can
    // still only ever speak for itself.
    if let Err(AssertionError::UnknownKey { .. }) =
        assertion::verify_claims(assertion_in, &verify_input)
    {
        fetch_peer_assertion_keys(from_node_id, iroh).await;
    }

    let verified = assertion::verify(&state.db, assertion_in, &verify_input)
        .map_err(|e| assertion_denied(&e))?;

    // The assertion is bound to these exact arguments twice over: by the digest
    // (checked above) and by the operation identity, which both nodes derive
    // from the same bytes.
    let expected_op = remote_op_id(&verified.session_id, payload_cbor);
    if verified.op_id != expected_op {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "assertion operation id does not match its payload",
        ));
    }

    let payload = decode(payload_cbor)?;

    // §12.1 — an irreversible operation does not get to ride out the assertion
    // lifetime. Ask the issuer to re-resolve the permission right now; anything
    // other than a clear "still permitted" stops the operation here.
    if let Some(capability) = irreversible_capability(&payload) {
        confirm_permission_freshness(&verified, capability, iroh).await?;
    }

    // The ONE local implementation, reached with a context built from local
    // state. `code_studio_dispatch` is `#[policy(UserSession)]`, and the context
    // built above is a `UserSession` by construction — the tier the frame
    // dispatcher would have enforced is satisfied here by the assertion having
    // verified, not skipped. Everything the handler does next — read
    // permission, membership, workspace role, PEP, containment — happens
    // exactly as it does for a caller connected to this node.
    let ctx = handler_context(&state, &verified)?;
    let response = with_remote_origin(
        assertion::digest_hex(payload_cbor),
        crate::dispatch::code_studio::code_studio_dispatch(
            &MessageBody::CodeStudioBody(payload),
            &ctx,
        ),
    )
    .await?;
    match response {
        MessageBody::CodeStudioBody(body) => encode(&body),
        _ => Err(ProtocolError::internal(
            "code studio handler returned a foreign message body",
        )),
    }
}

/// Rebuilds the caller's request context on this node from LOCAL data only.
///
/// The wire supplies the user id and nothing else that matters: the role comes
/// from this node's account row and the org context from this node's RBAC
/// tables, so a peer cannot describe the actor as more privileged than this
/// node believes them to be.
fn handler_context(
    state: &Arc<AppState>,
    verified: &assertion::VerifiedAssertion,
) -> Result<HandlerContext, ProtocolError> {
    let user_uuid = uuid::Uuid::parse_str(&verified.user_id).map_err(|_| {
        ProtocolError::new(
            ProtocolErrorCode::AuthRequired,
            "assertion subject is not a user id",
        )
    })?;
    let account = crate::db::repository::get_user_account_by_id(&state.db, &verified.user_id)
        .map_err(|e| ProtocolError::internal(format!("user lookup failed: {e}")))?
        .ok_or_else(|| {
            ProtocolError::new(
                ProtocolErrorCode::AuthRequired,
                "assertion subject is unknown on this node",
            )
        })?;
    if !account.is_active {
        return Err(ProtocolError::new(
            ProtocolErrorCode::AuthRequired,
            "assertion subject is deactivated on this node",
        ));
    }
    let org_context = crate::services::rbac::resolve_org_context(
        &state.db,
        &verified.user_id,
        Some(&verified.org_id),
    )
    .map_err(|e| ProtocolError::new(ProtocolErrorCode::AuthRequired, e.to_string()))?;

    Ok(HandlerContext {
        session: SessionAuth::UserSession {
            user_id: *user_uuid.as_bytes(),
            role: Some(account.role),
        },
        correlation_id: correlation_of(&verified.session_id, &verified.op_id),
        connection_id: 0,
        resume_secret: None,
        state: state.clone(),
        org_context: Some(org_context),
    })
}

/// A stable correlation id for the forwarded call, so the owner node's trace
/// lines can be joined with the caller's without inventing a counter.
fn correlation_of(session_id: &str, op: &str) -> u64 {
    let digest = assertion::digest_hex(format!("{session_id}:{op}").as_bytes());
    u64::from_str_radix(&digest[..16], 16).unwrap_or(0)
}

/// One round trip that asks the issuing node whether the actor may still do
/// this, right now, against ITS live database.
///
/// What this buys, stated honestly: a revocation performed on the owner node
/// takes effect immediately (the handler re-authorizes locally on every call),
/// and a revocation performed on the ISSUING node takes effect immediately too,
/// because of this probe. A revocation performed on some third node still waits
/// for the sync to converge, bounded by the assertion lifetime. There is no
/// arrangement of two round trips that fixes the third case.
async fn confirm_permission_freshness(
    verified: &assertion::VerifiedAssertion,
    capability: Capability,
    iroh: &Arc<IrohMeshManager>,
) -> Result<(), ProtocolError> {
    let response = iroh
        .send_command_and_wait(
            &verified.issuer_node_id,
            MeshCommandType::CodeStudioPermissionProbe {
                user_id: verified.user_id.clone(),
                org_id: verified.org_id.clone(),
                workspace_id: verified.workspace_id.clone(),
                capability: capability.slug().to_string(),
            },
            PROBE_TIMEOUT_SECS,
        )
        .await
        .map_err(|e| {
            ProtocolError::new(
                ProtocolErrorCode::NotAvailable,
                format!(
                    "{} needs a fresh permission check and node '{}' did not answer: {e}",
                    capability.slug(),
                    verified.issuer_node_id
                ),
            )
        })?;

    match response.payload {
        MeshCommandResponsePayload::CodeStudioPermissionProbeResult {
            permitted, reason, ..
        } => {
            if permitted {
                Ok(())
            } else {
                Err(ProtocolError::new(
                    ProtocolErrorCode::PolicyDenied,
                    format!(
                        "{} was refused by a fresh permission check on node '{}': {reason}",
                        capability.slug(),
                        verified.issuer_node_id
                    ),
                ))
            }
        }
        _ => Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!(
                "{} needs a fresh permission check and node '{}' answered something else",
                capability.slug(),
                verified.issuer_node_id
            ),
        )),
    }
}

/// Answer a peer's freshness probe from THIS node's live state.
///
/// Deliberately re-resolves everything instead of reading any cache: the whole
/// point of the probe is that it sees a change made a second ago.
pub fn answer_permission_probe(
    db: &crate::db::DbPool,
    user_id: &str,
    org_id: &str,
    workspace_id: &str,
    capability: &str,
) -> MeshCommandResponsePayload {
    let deny = |reason: String, rbac_rev: String, role: String| {
        MeshCommandResponsePayload::CodeStudioPermissionProbeResult {
            permitted: false,
            rbac_rev,
            workspace_role: role,
            reason,
        }
    };
    let Some(capability) = Capability::from_slug(capability) else {
        return deny(
            format!("unknown capability '{capability}'"),
            String::new(),
            String::new(),
        );
    };
    let role = match repository::role_of(db, workspace_id, user_id) {
        Ok(role) => role,
        Err(e) => {
            return deny(
                format!("membership lookup failed: {e}"),
                String::new(),
                String::new(),
            )
        }
    };
    let rbac_rev = assertion::rbac_revision(db, user_id, org_id, workspace_id).unwrap_or_default();
    let role_slug = role.map(|r| r.slug().to_string()).unwrap_or_default();

    let permitted = assertion::caps_for_role(role).contains(&capability);
    MeshCommandResponsePayload::CodeStudioPermissionProbeResult {
        permitted,
        rbac_rev,
        workspace_role: role_slug,
        reason: if permitted {
            String::new()
        } else {
            format!("{} is not held on this workspace", capability.slug())
        },
    }
}

// =============================================================================
// Assertion key distribution
// =============================================================================

/// Pull the issuer's current assertion keys after meeting an unknown `kid`.
/// Failure is not fatal here — verification simply reports `UnknownKey` and the
/// operation is refused.
async fn fetch_peer_assertion_keys(peer_id: &str, iroh: &Arc<IrohMeshManager>) {
    let response = iroh
        .send_command_and_wait(
            peer_id,
            MeshCommandType::CodeStudioAssertionKeysGet,
            PROBE_TIMEOUT_SECS,
        )
        .await;
    match response {
        Ok(response) => {
            if let MeshCommandResponsePayload::CodeStudioAssertionKeysResult { keys } =
                response.payload
            {
                let accepted = assertion::ingest_peer_keys(peer_id, &keys);
                tracing::debug!(
                    peer = peer_id,
                    accepted,
                    "code studio: pulled assertion keys"
                );
            }
        }
        Err(e) => {
            tracing::warn!(peer = peer_id, error = %e, "code studio: assertion key pull failed");
        }
    }
}

/// Push this node's assertion keys to every trust-paired peer. Called after a
/// rotation so peers learn the new `kid` before the next assertion signed with
/// it arrives; the pull above is the safety net for peers that were offline.
///
/// Same audience and same lifecycle as the HMAC issuer key mirror
/// (`services::mesh_keys`): trust-paired peers only, public material only, and
/// nothing survives a trust revocation because nothing is written to disk.
pub async fn broadcast_assertion_keys(
    iroh: &Arc<IrohMeshManager>,
    security: &Arc<crate::mesh::security::MeshSecurity>,
) -> usize {
    let keys = assertion::local_advertise();
    let peers = security.trusted_node_ids_snapshot();
    let mut delivered = 0usize;
    for peer in peers.iter() {
        let sent = iroh
            .send_command_and_wait(
                peer,
                MeshCommandType::CodeStudioAssertionKeysPush { keys: keys.clone() },
                PROBE_TIMEOUT_SECS,
            )
            .await;
        match sent {
            Ok(_) => delivered += 1,
            Err(e) => {
                tracing::debug!(peer = %peer, error = %e, "code studio: assertion key push failed")
            }
        }
    }
    delivered
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::TempDir;

    const ORG: &str = "org-shared";

    fn seeded_pool(dir: &TempDir, name: &str, user_id: &str) -> crate::db::DbPool {
        let pool = crate::db::init(&dir.path().join(name)).expect("init db");
        let admin_role = crate::services::org::repo::list_roles(&pool)
            .expect("roles")
            .into_iter()
            .find(|r| r.name == "org_admin")
            .expect("org_admin")
            .role_id;
        let conn = pool.write().expect("db");
        conn.execute(
            "INSERT OR IGNORE INTO organizations (org_id, name, slug, status, created_at) \
             VALUES (?1, 'Shared', ?1, 'active', datetime('now'))",
            params![ORG],
        )
        .expect("org");
        conn.execute(
            "INSERT OR IGNORE INTO user_accounts \
               (id, username, password_hash, display_name, email, is_active, is_admin, \
                created_at, updated_at, role) \
             VALUES (?1, ?1, 'x', ?1, ?1, 1, 0, datetime('now'), datetime('now'), 'user')",
            params![user_id],
        )
        .expect("user");
        conn.execute(
            "INSERT OR IGNORE INTO org_memberships \
               (org_id, user_id, role_id, granted_at, granted_by) \
             VALUES (?1, ?2, ?3, datetime('now'), ?2)",
            params![ORG, user_id, admin_role],
        )
        .expect("org membership");
        conn.execute(
            "INSERT OR IGNORE INTO code_workspaces \
               (id, org_id, owner_user_id, name, slug, node_id, exec_mode, \
                egress_enforcement, repo_kind, autonomy_ceiling, egress_policy, \
                index_enabled, status, created_at, updated_at) \
             VALUES ('ws-1', ?2, ?1, 'W', 'w', 'node-b', 'trusted_native', 'unrestricted', \
                'empty', 'normal', 'org_approved', 0, 'active', datetime('now'), datetime('now'))",
            params![user_id, ORG],
        )
        .expect("workspace");
        conn.execute(
            "INSERT OR REPLACE INTO code_workspace_members \
               (workspace_id, user_id, role, added_by, added_at) \
             VALUES ('ws-1', ?1, 'owner', ?1, datetime('now'))",
            params![user_id],
        )
        .expect("membership");
        drop(conn);
        pool
    }

    fn permitted(payload: &MeshCommandResponsePayload) -> bool {
        match payload {
            MeshCommandResponsePayload::CodeStudioPermissionProbeResult { permitted, .. } => {
                *permitted
            }
            _ => panic!("expected a probe result"),
        }
    }

    /// The freshness probe of §12.1: the issuing node re-resolves the actor's
    /// permission against its LIVE database, so a role taken away there stops a
    /// `git_push` on the owner node even though the assertion is still valid.
    #[test]
    fn git_push_is_refused_by_the_freshness_probe_after_a_remote_revocation() {
        let dir = TempDir::new().expect("tempdir");
        let user_id = uuid::Uuid::new_v4().to_string();
        let pool = seeded_pool(&dir, "issuer.db", &user_id);

        let before = answer_permission_probe(&pool, &user_id, ORG, "ws-1", "git_push");
        assert!(
            permitted(&before),
            "an owner may push before the revocation"
        );

        {
            let conn = pool.write().expect("db");
            conn.execute(
                "UPDATE code_workspace_members SET role = 'editor' WHERE user_id = ?1",
                params![user_id],
            )
            .expect("demote");
        }

        let after = answer_permission_probe(&pool, &user_id, ORG, "ws-1", "git_push");
        assert!(
            !permitted(&after),
            "the probe must see the demotion that happened a moment ago"
        );
        // An editor keeps the capabilities an editor has — the probe is a
        // per-capability answer, not a blanket denial.
        assert!(permitted(&answer_permission_probe(
            &pool, &user_id, ORG, "ws-1", "fs_write"
        )));
    }

    #[test]
    fn a_probe_for_a_non_member_is_refused() {
        let dir = TempDir::new().expect("tempdir");
        let user_id = uuid::Uuid::new_v4().to_string();
        let pool = seeded_pool(&dir, "issuer.db", &user_id);
        let answer = answer_permission_probe(&pool, "someone-else", ORG, "ws-1", "fs_read");
        assert!(!permitted(&answer));
    }

    #[test]
    fn remote_op_id_is_stable_for_the_same_call_and_differs_otherwise() {
        let a = remote_op_id("sess-1", b"payload");
        let b = remote_op_id("sess-1", b"payload");
        let c = remote_op_id("sess-1", b"payload!");
        let d = remote_op_id("sess-2", b"payload");
        assert_eq!(a, b, "a retry of the same call must reuse the operation id");
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    #[test]
    fn only_the_mandatory_interactive_operations_take_the_freshness_probe() {
        assert_eq!(
            irreversible_capability(&CodeStudioPayload::GitPushRequest {
                workspace_id: "ws".into(),
                session_id: "sess".into(),
                remote: String::new(),
                set_upstream: false,
            }),
            Some(Capability::GitPush)
        );
        assert_eq!(
            irreversible_capability(&CodeStudioPayload::SessionsListRequest {
                workspace_id: "ws".into(),
            }),
            None
        );
    }
}
