// ===== File: dispatch/app_route.rs — platform node routing (plan §3.1) =====
//
// An app-family dashboard request may be addressed to another fleet node
// (`Routing::Forward` in the envelope): an admin manages ANY node from ANY
// dashboard, and hardware-facing apps (TentaNas) act on the node that owns the
// hardware. The node showing the dashboard is a proxy with NO decision logic:
// it mints a `SessionAssertion` for the person acting, sends the request's
// CBOR bytes verbatim over the trusted mesh, and relays whatever the executing
// node answered.
//
// The executing node verifies the assertion (identity only, never authority),
// rebuilds the actor's context from ITS OWN database — account row, role, org
// membership, permission matrix — and runs the ordinary `dispatch()` pipeline,
// so every gate (`#[policy]` tier, app gate, matrix check) applies exactly as
// if the request had arrived locally. Blind trust in the forwarding node is
// forbidden by design: a compromised peer must not gain more than its own
// local authority.
//
// This module transports `MessageBody` values, one request/response pair per
// call. Streaming subscriptions are NOT forwardable here — they need a relayed
// stream (mesh stream hub), not a oneshot.

use std::sync::Arc;

use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType, SessionAssertion};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use crate::code_studio::assertion;
use crate::code_studio::remote_proxy;
use crate::dispatch::HandlerContext;
use crate::mesh::iroh_manager::IrohMeshManager;

/// How long the origin waits for the executing node to run one request. App
/// requests can cold-start a native app or a WASM instance, so this mirrors
/// Code Studio's figure rather than the short command timeouts.
const OP_TIMEOUT_SECS: u64 = 45;

/// The assertion's `workspace` field for app routing. Domain separation: an
/// assertion minted for a Code Studio workspace must never authorize an
/// `AppRouteOp` (and vice versa) even if its payload bytes happened to decode
/// both ways — the executing side rejects any other value.
const APP_ROUTE_SCOPE: &str = "app-route";

fn unreachable(node_id: &str, detail: impl std::fmt::Display) -> ProtocolError {
    // Connectivity, not state: nothing about this is persisted.
    ProtocolError::new(
        ProtocolErrorCode::NodeUnreachable,
        format!("node '{node_id}' did not answer: {detail}"),
    )
}

fn decode_body(bytes: &[u8]) -> Result<MessageBody, ProtocolError> {
    tentaflow_protocol::cbor::decode(bytes)
        .map_err(|e| ProtocolError::bad_request(format!("message body decode failed: {e}")))
}

fn encode_body(body: &MessageBody) -> Result<Vec<u8>, ProtocolError> {
    tentaflow_protocol::cbor::encode(body)
        .map_err(|e| ProtocolError::internal(format!("message body encode failed: {e}")))
}

// =============================================================================
// Origin side — the proxy
// =============================================================================

/// Forward one dashboard request (the envelope's raw `MessageBody` CBOR) to
/// `node_id` and return the executing node's response body.
///
/// `body_cbor` travels verbatim — the assertion's `args_digest` binds those
/// exact bytes, and the executing node decodes them with the same decoder the
/// local dispatcher would have used. The caller has already established that
/// the target is not the local node.
pub async fn forward_to_node(
    ctx: &HandlerContext,
    node_id: &str,
    body_cbor: Vec<u8>,
) -> Result<MessageBody, ProtocolError> {
    // Loop guard: a request already executing on behalf of another node must
    // not fan out again — two nodes disagreeing about a target would bounce
    // the request until the deadline.
    if remote_proxy::current_remote_origin_id().is_some() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "a forwarded request cannot be forwarded again",
        ));
    }
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

    let caps = assertion::resolve_caps(&ctx.state.db, &org.user_id, APP_ROUTE_SCOPE)
        .map_err(|e| remote_proxy::assertion_denied(&e))?;
    let rbac_rev =
        assertion::rbac_revision(&ctx.state.db, &org.user_id, &org.org_id, APP_ROUTE_SCOPE)
            .map_err(|e| remote_proxy::assertion_denied(&e))?;
    let op = remote_proxy::remote_op_id("", &body_cbor);
    let minted = assertion::issue(&assertion::IssueRequest {
        local_node_id: &ctx.state.local_node_id,
        user_id: &org.user_id,
        owner_node_id: node_id,
        org_id: &org.org_id,
        workspace_id: APP_ROUTE_SCOPE,
        session_id: "",
        caps: &caps,
        rbac_rev: &rbac_rev,
        op_id: &op,
        payload_cbor: &body_cbor,
        lifetime_ms: assertion::DEFAULT_LIFETIME_MS,
    })
    .map_err(|e| remote_proxy::assertion_denied(&e))?;

    // Origin-side audit: the executing node writes the authoritative entry
    // (with its own gate outcome); this records WHERE the user sent it from.
    let _ = crate::db::repository::log_audit_full(
        &ctx.state.db,
        Some(&org.user_id),
        None,
        "app.route.forward",
        Some("mesh_node"),
        Some(node_id),
        None,
        "info",
        "B",
        None,
        Some(&org.org_id),
        None,
        Some(&ctx.state.local_node_id),
    );

    let response = iroh
        .send_command_and_wait(
            node_id,
            MeshCommandType::AppRouteOp {
                assertion: minted,
                payload_cbor: body_cbor,
            },
            OP_TIMEOUT_SECS,
        )
        .await
        .map_err(|e| unreachable(node_id, e))?;

    let (payload, transport_error) = (response.payload, response.error);
    match payload {
        MeshCommandResponsePayload::AppRouteOpResult {
            payload_cbor,
            error,
        } => match error {
            Some(error) => Err(error),
            None => decode_body(&payload_cbor),
        },
        _ => Err(unreachable(
            node_id,
            transport_error.unwrap_or_else(|| "unexpected mesh response payload".to_string()),
        )),
    }
}

// =============================================================================
// Executing side
// =============================================================================

pub async fn execute_remote_side(
    from_node_id: &str,
    assertion_in: &SessionAssertion,
    payload_cbor: &[u8],
    iroh: &Arc<IrohMeshManager>,
) -> (Vec<u8>, Option<ProtocolError>) {
    match execute_remote_side_inner(from_node_id, assertion_in, payload_cbor, iroh).await {
        Ok(bytes) => (bytes, None),
        Err(error) => (Vec::new(), Some(error)),
    }
}

async fn execute_remote_side_inner(
    from_node_id: &str,
    assertion_in: &SessionAssertion,
    payload_cbor: &[u8],
    iroh: &Arc<IrohMeshManager>,
) -> Result<Vec<u8>, ProtocolError> {
    let state = remote_proxy::node_state().cloned().ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "mesh execution context is not initialized on this node",
        )
    })?;

    let verify_input = assertion::VerifyInput {
        channel_peer_id: from_node_id,
        local_node_id: &state.local_node_id,
        payload_cbor,
        now_unix_ms: crate::services::mesh_keys::now_unix_ms(),
    };
    // An unknown key is the ordinary case right after the issuer restarted or
    // rotated — pull its ring once from the SAME authenticated peer, then
    // verify for real.
    if let Err(assertion::AssertionError::UnknownKey { .. }) =
        assertion::verify_claims(assertion_in, &verify_input)
    {
        remote_proxy::fetch_peer_assertion_keys(from_node_id, iroh).await;
    }
    let verified = assertion::verify(&state.db, assertion_in, &verify_input)
        .map_err(|e| remote_proxy::assertion_denied(&e))?;

    // Domain separation: this assertion must have been minted FOR app routing.
    if verified.workspace_id != APP_ROUTE_SCOPE {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "assertion scope does not authorize app routing",
        ));
    }
    let expected_op = remote_proxy::remote_op_id(&verified.session_id, payload_cbor);
    if verified.op_id != expected_op {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "assertion operation id does not match its payload",
        ));
    }

    let body = decode_body(payload_cbor)?;
    let variant_name = crate::dispatch::variant_name_of(&body);

    // A streaming subscription has no oneshot response to relay — refuse it
    // clearly instead of letting `dispatch()` report a missing handler.
    if crate::dispatch::subscription::find_stream_handler(variant_name).is_some() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "streaming subscriptions cannot be forwarded between nodes",
        ));
    }

    // Identity rebuilt from LOCAL data only (account row, role, org RBAC) —
    // the wire supplied the user id and nothing that grants anything.
    let ctx = remote_proxy::handler_context(&state, &verified)?;

    // Executor-side audit with the REAL user, not "mesh peer".
    let _ = crate::db::repository::log_audit_full(
        &state.db,
        Some(&verified.user_id),
        None,
        "app.route.execute",
        Some("mesh_node"),
        Some(from_node_id),
        Some(variant_name),
        "info",
        "B",
        None,
        Some(&verified.org_id),
        None,
        Some(&state.local_node_id),
    );

    let (response, _is_error) = remote_proxy::with_remote_origin(
        assertion::digest_hex(payload_cbor),
        crate::dispatch::dispatch(&body, &ctx),
    )
    .await;
    match response {
        // The refusal travels as a typed error so the origin node relays the
        // code (PolicyDenied / AppUnavailable / NotFound...) unchanged.
        MessageBody::Error(error) => Err(error),
        other => encode_body(&other),
    }
}
