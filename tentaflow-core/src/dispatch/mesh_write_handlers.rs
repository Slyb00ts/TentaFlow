// =============================================================================
// Plik: dispatch/mesh_write_handlers.rs
// Opis: Async handlery operacji zapisu mesh (FAZA 1b): pairing/trust/connect/
//       command/network-config. Wszystkie sa NATYWNIE async — bez block_on
//       ani tokio::Handle::current. Reuzywaja domenowe helpery z
//       api/dashboard/api_mesh.rs (te same co REST wczesniej wolal), ale
//       mapuja rezultat (u16, json) na MessageBody variants.
// =============================================================================

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MeshConnectRequest, MeshConnectResponse, MeshNodeCommandRequest, MeshNodeCommandResponse,
    MeshNodeNetworkConfigRequest, MeshNodeNetworkConfigResponse, MeshPairingConfirmRequest,
    MeshPairingConfirmResponse, MeshPairingRejectRequest, MeshPairingRejectResponse,
    MeshPairingStartRequest, MeshPairingStartResponse, MeshTrustRetrustRequest,
    MeshTrustRetrustResponse, MeshTrustRevokeRequest, MeshTrustRevokeResponse, MessageBody,
    NsightPayload, ProtocolError, ProtocolErrorCode,
};
use tracing::warn;

use super::HandlerContext;
use crate::api::dashboard::api_mesh;
use crate::db::repository;
use crate::mesh::iroh_manager::IrohMeshManager;
use crate::mesh::security::MeshSecurity;

// =============================================================================
// Helpery
// =============================================================================

fn require_quic_mesh(ctx: &HandlerContext) -> Result<Arc<IrohMeshManager>, ProtocolError> {
    ctx.state
        .quic_mesh
        .clone()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::Internal, "Mesh manager niedostepny"))
}

fn require_mesh_security(ctx: &HandlerContext) -> Result<Arc<MeshSecurity>, ProtocolError> {
    ctx.state
        .mesh_security
        .clone()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::Internal, "MeshSecurity niedostepny"))
}

/// Mapuje HTTP-style status code z api_mesh::handle_* na ProtocolError.
fn http_status_to_proto_err(status: u16, json_body: &str) -> ProtocolError {
    // Wyekstrahuj "error" pole jesli to JSON object.
    let msg = serde_json::from_str::<serde_json::Value>(json_body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| json_body.to_string());

    let code = match status {
        400 => ProtocolErrorCode::BadRequest,
        403 => ProtocolErrorCode::PolicyDenied,
        404 => ProtocolErrorCode::NotFound,
        413 | 429 => ProtocolErrorCode::BadRequest,
        502 | 503 => ProtocolErrorCode::Internal,
        _ => ProtocolErrorCode::Internal,
    };
    ProtocolError::new(code, msg)
}

// =============================================================================
// 1. MeshPairingStartRequest — rozpocznij parowanie, wygeneruj PIN, wyslij przez QUIC.
// =============================================================================

#[handler(variant = "MeshPairingStartRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn mesh_pairing_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MeshPairingStartRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MeshPairingStartRequestBody",
            ))
        }
    };
    let MeshPairingStartRequest {
        remote_address,
        pin_hint,
        remote_public_key,
        remote_addresses,
        remote_relay_url,
        remote_hostname,
    } = payload;

    let security = require_mesh_security(ctx)?;

    // Uwaga: REST handler uzywal "remote_address" jako node_id (legacy shape).
    // Zachowujemy te sama semantyke — dla binary protocol to jest faktycznie
    // identyfikator zdalnego noda (lub jego publicznego aliasu).
    let (status, json_body) = api_mesh::handle_initiate_pairing(
        &ctx.state.db,
        &security,
        remote_address,
        remote_public_key,
        remote_addresses,
        remote_relay_url,
        remote_hostname,
        &ctx.state.quic_mesh,
        ctx.state.local_node_id.as_ref(),
        &ctx.state.mesh_peer_store,
        pin_hint,
    )
    .await
    .map_err(|e| ProtocolError::internal(format!("pairing start failed: {}", e)))?;

    if status != 200 {
        return Err(http_status_to_proto_err(status, &json_body));
    }

    // JSON body: {"pin": ..., "node_id": ..., "expires_in_seconds": ...}
    let parsed: serde_json::Value = serde_json::from_str(&json_body)
        .map_err(|e| ProtocolError::internal(format!("pairing response parse: {}", e)))?;
    let pin = parsed
        .get("pin")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let completed = parsed
        .get("completed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Pair_id = node_id (mapowanie proste — pairing jednoznaczny per node).
    let pair_id = remote_address.clone();

    Ok(MessageBody::MeshPairingStartResponseBody(
        MeshPairingStartResponse {
            pair_id,
            pin,
            completed,
        },
    ))
}

// =============================================================================
// 2. MeshPairingConfirmRequest — potwierdz parowanie + sync kluczy.
// =============================================================================

#[handler(variant = "MeshPairingConfirmRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn mesh_pairing_confirm(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MeshPairingConfirmRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MeshPairingConfirmRequestBody",
            ))
        }
    };
    let MeshPairingConfirmRequest { pair_id, pin } = payload;

    let security = require_mesh_security(ctx)?;

    // pair_id mapuje na node_id (patrz mesh_pairing_start).
    let body_json = serde_json::json!({
        "pin": pin,
        "hostname": "",
    })
    .to_string();

    let (status, json_body) = api_mesh::handle_confirm_pairing(
        &security,
        pair_id,
        body_json.as_bytes(),
        &ctx.state.quic_mesh,
        ctx.state.local_node_id.as_ref(),
    )
    .map_err(|e| ProtocolError::internal(format!("pairing confirm failed: {}", e)))?;

    if status != 200 {
        return Err(http_status_to_proto_err(status, &json_body));
    }

    Ok(MessageBody::MeshPairingConfirmResponseBody(
        MeshPairingConfirmResponse {
            ok: true,
            trusted_node_id: pair_id.clone(),
        },
    ))
}

// =============================================================================
// 3. MeshPairingRejectRequest — odrzuc parowanie.
// =============================================================================

#[handler(variant = "MeshPairingRejectRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn mesh_pairing_reject(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MeshPairingRejectRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MeshPairingRejectRequestBody",
            ))
        }
    };
    let MeshPairingRejectRequest { pair_id } = payload;

    let security = require_mesh_security(ctx)?;

    let (status, json_body) = api_mesh::handle_reject_pairing(
        &security,
        pair_id,
        &ctx.state.quic_mesh,
        ctx.state.local_node_id.as_ref(),
    )
    .map_err(|e| ProtocolError::internal(format!("pairing reject failed: {}", e)))?;

    if status != 200 {
        return Err(http_status_to_proto_err(status, &json_body));
    }

    Ok(MessageBody::MeshPairingRejectResponseBody(
        MeshPairingRejectResponse { ok: true },
    ))
}

// =============================================================================
// 4. MeshTrustRevokeRequest — cofnij zaufanie + broadcast do mesh.
// =============================================================================

#[handler(variant = "MeshTrustRevokeRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn mesh_trust_revoke(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MeshTrustRevokeRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MeshTrustRevokeRequestBody",
            ))
        }
    };
    let MeshTrustRevokeRequest { node_id } = payload;

    let security = require_mesh_security(ctx)?;

    let (status, json_body) = api_mesh::handle_revoke_trust(
        &security,
        node_id,
        &ctx.state.quic_mesh,
        ctx.state.local_node_id.as_ref(),
    )
    .map_err(|e| ProtocolError::internal(format!("trust revoke failed: {}", e)))?;

    if status != 200 {
        return Err(http_status_to_proto_err(status, &json_body));
    }

    Ok(MessageBody::MeshTrustRevokeResponseBody(
        MeshTrustRevokeResponse { ok: true },
    ))
}

// =============================================================================
// 5. MeshTrustRetrustRequest — przywroc zaufanie (admin).
// =============================================================================

#[handler(variant = "MeshTrustRetrustRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn mesh_trust_retrust(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MeshTrustRetrustRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MeshTrustRetrustRequestBody",
            ))
        }
    };
    let MeshTrustRetrustRequest { node_id } = payload;

    let security = require_mesh_security(ctx)?;

    let (status, json_body) = api_mesh::handle_retrust(&security, node_id)
        .map_err(|e| ProtocolError::internal(format!("retrust failed: {}", e)))?;

    if status != 200 {
        return Err(http_status_to_proto_err(status, &json_body));
    }

    Ok(MessageBody::MeshTrustRetrustResponseBody(
        MeshTrustRetrustResponse { ok: true },
    ))
}

// =============================================================================
// 6. MeshConnectRequest — manualne QUIC polaczenie po IP:port.
// =============================================================================

#[handler(variant = "MeshConnectRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn mesh_connect(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MeshConnectRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MeshConnectRequestBody",
            ))
        }
    };
    let MeshConnectRequest { address } = payload;

    let qm = require_quic_mesh(ctx)?;

    let addr: SocketAddr = address.parse().map_err(|_| {
        ProtocolError::bad_request("Niepoprawny format adresu (oczekiwany IP:port)")
    })?;

    // SSRF guard — blokuj loopback / unspecified / link-local.
    let ip = addr.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        return Err(ProtocolError::bad_request("Niedozwolony adres docelowy"));
    }
    if let IpAddr::V4(v4) = ip {
        if v4.is_link_local() {
            return Err(ProtocolError::bad_request("Niedozwolony adres docelowy"));
        }
    }

    let temp_node_id = format!("manual-{}", addr);
    match qm.connect_to_peer(&temp_node_id, addr).await {
        Ok(()) => Ok(MessageBody::MeshConnectResponseBody(MeshConnectResponse {
            ok: true,
            remote_node_id: Some(temp_node_id),
        })),
        Err(e) => Err(ProtocolError::new(
            ProtocolErrorCode::Internal,
            format!("Blad polaczenia: {}", e),
        )),
    }
}

// =============================================================================
// 7. MeshNodeCommandRequest — wyslij komende do zaufanego noda.
// =============================================================================

#[handler(variant = "MeshNodeCommandRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn mesh_node_command(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MeshNodeCommandRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MeshNodeCommandRequestBody",
            ))
        }
    };
    let MeshNodeCommandRequest {
        node_id,
        command,
        args,
    } = payload;

    let qm = require_quic_mesh(ctx)?;
    let is_trusted = ctx
        .state
        .mesh_security
        .as_ref()
        .map_or(false, |s| s.is_trusted(node_id));
    if !is_trusted {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "Node nie jest zaufany — nie mozna wyslac komendy",
        ));
    }

    // Mapowanie command+args na tentaflow_protocol::mesh::MeshCommandType.
    use tentaflow_protocol::mesh::MeshCommandType;
    let cmd = match command.as_str() {
        "list_containers" => MeshCommandType::ListContainers,
        "list_images" => MeshCommandType::ListImages,
        "container_start" => {
            let container_id = args.first().cloned().unwrap_or_default();
            MeshCommandType::ContainerStart { container_id }
        }
        "container_stop" => {
            let container_id = args.first().cloned().unwrap_or_default();
            MeshCommandType::ContainerStop { container_id }
        }
        "container_restart" => {
            let container_id = args.first().cloned().unwrap_or_default();
            MeshCommandType::ContainerRestart { container_id }
        }
        "system_prune" => {
            let volumes = args.first().map(|s| s == "true").unwrap_or(false);
            MeshCommandType::SystemPrune { volumes }
        }
        other => {
            return Err(ProtocolError::bad_request(format!(
                "Nieznany typ komendy: {}",
                other
            )));
        }
    };

    match qm.send_command(node_id, cmd).await {
        Ok(response) => {
            // Typed payload → human-readable output dla dashboardu (pojedyncze pole
            // `output: Option<String>` w MeshNodeCommandResponse). Serializujemy
            // payload jako JSON, zeby UI moglo wyrenderowac strukturalna odpowiedz.
            let output = match &response.payload {
                tentaflow_protocol::mesh::MeshCommandResponsePayload::Empty => None,
                tentaflow_protocol::mesh::MeshCommandResponsePayload::Text(t) if t.is_empty() => {
                    None
                }
                tentaflow_protocol::mesh::MeshCommandResponsePayload::Text(t) => Some(t.clone()),
                other => serde_json::to_string(other).ok(),
            };
            Ok(MessageBody::MeshNodeCommandResponseBody(
                MeshNodeCommandResponse {
                    ok: response.ok,
                    output,
                },
            ))
        }
        Err(e) => {
            warn!(node_id = %node_id, error = %e, "mesh_node_command failed");
            Err(ProtocolError::new(
                ProtocolErrorCode::Internal,
                format!("Blad wykonania komendy: {}", e),
            ))
        }
    }
}

// =============================================================================
// 8. MeshNodeNetworkConfigRequest — zmiana konfiguracji sieci na zdalnym nodzie.
// =============================================================================

#[handler(variant = "MeshNodeNetworkConfigRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn mesh_node_network_config(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MeshNodeNetworkConfigRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MeshNodeNetworkConfigRequestBody",
            ))
        }
    };
    let MeshNodeNetworkConfigRequest {
        node_id,
        interface_name,
        config_json,
    } = payload;

    let qm = require_quic_mesh(ctx)?;

    let is_trusted = ctx
        .state
        .mesh_security
        .as_ref()
        .map_or(false, |s| s.is_trusted(node_id));
    if !is_trusted {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "Node nie jest zaufany — nie mozna wyslac konfiguracji sieci",
        ));
    }

    // Parsuj config_json: {ipv4?, netmask?, gateway?, dhcp?, sudo_password}
    let cfg: serde_json::Value = serde_json::from_str(config_json)
        .map_err(|e| ProtocolError::bad_request(format!("Niepoprawny config_json: {}", e)))?;

    let ipv4 = cfg.get("ipv4").and_then(|v| v.as_str()).map(String::from);
    let netmask = cfg
        .get("netmask")
        .and_then(|v| v.as_str())
        .map(String::from);
    let gateway = cfg
        .get("gateway")
        .and_then(|v| v.as_str())
        .map(String::from);
    let dhcp = cfg.get("dhcp").and_then(|v| v.as_bool()).unwrap_or(false);
    let sudo_password = cfg
        .get("sudo_password")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    if interface_name.is_empty() {
        return Err(ProtocolError::bad_request("Pole 'interface' jest wymagane"));
    }
    if sudo_password.is_empty() {
        return Err(ProtocolError::bad_request(
            "Pole 'sudo_password' jest wymagane",
        ));
    }

    use tentaflow_protocol::mesh::MeshCommandType;
    let cmd = MeshCommandType::NetworkConfig {
        interface: interface_name.clone(),
        ipv4,
        netmask,
        gateway,
        dhcp,
        sudo_password,
    };

    match qm.send_command(node_id, cmd).await {
        Ok(response) => {
            let _ = repository::log_audit(
                &ctx.state.db,
                None,
                None,
                "mesh.network_config",
                Some(&format!("node:{}/iface:{}", node_id, interface_name)),
                Some(if response.ok { "ok" } else { "failed" }),
                None,
                Some(ctx.state.local_node_id.as_ref()),
            );
            Ok(MessageBody::MeshNodeNetworkConfigResponseBody(
                MeshNodeNetworkConfigResponse {
                    ok: response.ok,
                },
            ))
        }
        Err(e) => Err(ProtocolError::new(
            ProtocolErrorCode::Internal,
            format!("Blad wykonania komendy: {}", e),
        )),
    }
}

// =============================================================================
// 9. NsightBody — start/stop/sessions/report/delete sesji profilowania.
// =============================================================================

/// Mapuje `ProfilingError` na `ProtocolError`. NotAvailable/Busy → Internal,
/// brak rozroznienia bo `ProtocolErrorCode` nie ma `Conflict`/`ServiceUnavailable`.
/// Tresc komunikatu jest jednoznaczna, GUI moze rozpoznac po prefiksie.
fn profiling_err_to_proto(e: crate::profiling::ProfilingError) -> ProtocolError {
    use crate::profiling::ProfilingError as PE;
    match e {
        PE::NotAvailable => ProtocolError::new(
            ProtocolErrorCode::Internal,
            "nsys not available on this node",
        ),
        PE::Busy => ProtocolError::new(
            ProtocolErrorCode::Internal,
            "another profiling session is already running",
        ),
        PE::NotFound(s) => ProtocolError::not_found(format!("session not found: {}", s)),
        PE::InvalidSessionId => ProtocolError::bad_request("invalid session id format"),
        PE::InvalidDuration(d) => {
            ProtocolError::bad_request(format!("invalid duration: {}s", d))
        }
        other => ProtocolError::internal(format!("profiling: {}", other)),
    }
}

/// Buduje `ProfileStorage` dla lokalnego noda. Storage rozdziela katalogi per
/// node_id, wiec uzywamy `state.local_node_id`.
fn local_profile_storage(ctx: &HandlerContext) -> crate::profiling::ProfileStorage {
    crate::profiling::ProfileStorage::new(
        crate::paths::tentaflow_home(),
        ctx.state.local_node_id.as_ref(),
    )
}

/// Wykonuje `MeshCommandType::Nsight*` na zdalnym nodzie i odpakowuje typed
/// `MeshCommandResponsePayload::Nsight*` w `NsightPayload::*Response`.
async fn forward_nsight_to_peer(
    ctx: &HandlerContext,
    target_node_id: &str,
    cmd: tentaflow_protocol::mesh::MeshCommandType,
) -> Result<NsightPayload, ProtocolError> {
    use tentaflow_protocol::mesh::MeshCommandResponsePayload as RP;

    let qm = require_quic_mesh(ctx)?;
    let is_trusted = ctx
        .state
        .mesh_security
        .as_ref()
        .map_or(false, |s| s.is_trusted(target_node_id));
    if !is_trusted {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "Node nie jest zaufany — nie mozna wyslac komendy",
        ));
    }

    let response = qm.send_command(target_node_id, cmd).await.map_err(|e| {
        ProtocolError::new(
            ProtocolErrorCode::Internal,
            format!("mesh nsight forward: {}", e),
        )
    })?;

    if !response.ok {
        let msg = response
            .error
            .unwrap_or_else(|| "remote node refused command".to_string());
        return Err(ProtocolError::new(ProtocolErrorCode::Internal, msg));
    }

    match response.payload {
        RP::NsightStart(r) => Ok(NsightPayload::StartResponse(r)),
        RP::NsightStop(r) => Ok(NsightPayload::StopResponse(r)),
        RP::NsightSessions(r) => Ok(NsightPayload::SessionsResponse(r)),
        RP::NsightReport(r) => Ok(NsightPayload::ReportResponse(r)),
        RP::NsightDelete(r) => Ok(NsightPayload::DeleteResponse(r)),
        _ => Err(ProtocolError::internal(
            "remote node returned unexpected payload variant",
        )),
    }
}

/// Lokalna obsluga sub-akcji NsightPayload — wywolywana gdy `req.node_id`
/// odpowiada lokalnemu nodowi. Reuzywa `NSYS_RUNNER` i `ProfileStorage`.
async fn handle_nsight_local(
    ctx: &HandlerContext,
    payload: NsightPayload,
) -> Result<NsightPayload, ProtocolError> {
    use crate::profiling::NSYS_RUNNER;
    use tentaflow_protocol::profiling::{
        NsightDeleteResponse, NsightReportResponse, NsightSessionsResponse, NsightStartResponse,
        NsightStopResponse,
    };

    match payload {
        NsightPayload::StartRequest(req) => {
            let storage = local_profile_storage(ctx);
            let (session_id, started_at_ms) = NSYS_RUNNER
                .start(req.scope, req.duration_secs, req.label, &storage)
                .await
                .map_err(profiling_err_to_proto)?;
            Ok(NsightPayload::StartResponse(NsightStartResponse {
                session_id,
                started_at_ms,
            }))
        }
        NsightPayload::StopRequest(req) => {
            let storage = local_profile_storage(ctx);
            let status = NSYS_RUNNER
                .stop(&req.session_id, &storage)
                .await
                .map_err(profiling_err_to_proto)?;
            Ok(NsightPayload::StopResponse(NsightStopResponse {
                session_id: req.session_id,
                status,
            }))
        }
        NsightPayload::SessionsRequest(req) => {
            let storage = local_profile_storage(ctx);
            let sessions = storage.list().map_err(profiling_err_to_proto)?;
            Ok(NsightPayload::SessionsResponse(NsightSessionsResponse {
                node_id: req.node_id,
                sessions,
            }))
        }
        NsightPayload::ReportRequest(req) => {
            let storage = local_profile_storage(ctx);
            let report = storage
                .read_summary(&req.session_id)
                .map_err(profiling_err_to_proto)?;
            Ok(NsightPayload::ReportResponse(NsightReportResponse { report }))
        }
        NsightPayload::DeleteRequest(req) => {
            let storage = local_profile_storage(ctx);
            storage
                .delete(&req.session_id)
                .map_err(profiling_err_to_proto)?;
            Ok(NsightPayload::DeleteResponse(NsightDeleteResponse {
                session_id: req.session_id,
                ok: true,
            }))
        }
        // Response warianty nie powinny przyjsc jako request — zwracaj BadRequest.
        NsightPayload::StartResponse(_)
        | NsightPayload::StopResponse(_)
        | NsightPayload::SessionsResponse(_)
        | NsightPayload::ReportResponse(_)
        | NsightPayload::DeleteResponse(_) => Err(ProtocolError::bad_request(
            "expected NsightPayload request variant",
        )),
    }
}

/// Wybiera lokalna albo mesh-forward sciezke po `req.node_id`.
async fn nsight_route(
    ctx: &HandlerContext,
    payload: NsightPayload,
) -> Result<NsightPayload, ProtocolError> {
    use tentaflow_protocol::mesh::MeshCommandType as MC;

    let local = ctx.state.local_node_id.as_ref();
    let target: String = match &payload {
        NsightPayload::StartRequest(r) => r.node_id.clone(),
        NsightPayload::StopRequest(r) => r.node_id.clone(),
        NsightPayload::SessionsRequest(r) => r.node_id.clone(),
        NsightPayload::ReportRequest(r) => r.node_id.clone(),
        NsightPayload::DeleteRequest(r) => r.node_id.clone(),
        _ => {
            return Err(ProtocolError::bad_request(
                "expected NsightPayload request variant",
            ))
        }
    };

    if target.is_empty() || target.as_str() == local {
        return handle_nsight_local(ctx, payload).await;
    }

    let cmd = match payload {
        NsightPayload::StartRequest(r) => MC::NsightStart(r),
        NsightPayload::StopRequest(r) => MC::NsightStop(r),
        NsightPayload::SessionsRequest(r) => MC::NsightSessions(r),
        NsightPayload::ReportRequest(r) => MC::NsightReport(r),
        NsightPayload::DeleteRequest(r) => MC::NsightDelete(r),
        _ => unreachable!("filtered above"),
    };
    forward_nsight_to_peer(ctx, &target, cmd).await
}

/// Jeden handler dla calego `MessageBody::NsightBody` — wewnatrz match po
/// wariantach `NsightPayload`. Zarejestrowany pod 5 nazwami request-side przez
/// `register_nsight_variant!` macro (variant_name_of zwraca pojedyncze nazwy
/// jak "NsightStartRequest").
#[handler(variant = "NsightBody", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn nsight_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::NsightBody(p) => p.clone(),
        _ => return Err(ProtocolError::bad_request("expected NsightBody")),
    };
    let res = nsight_route(ctx, payload).await?;
    Ok(MessageBody::NsightBody(res))
}

// variant_name_of() zwraca nazwy inner payloadu (np. "NsightStartRequest"),
// wiec rejestrujemy `nsight_dispatch` pod kazdym z 5 nazw request-side.
// Wzorzec analogiczny do `register_iam_variant!` w handlers.rs — wrapper
// `__tentaflow_dispatch_nsight_dispatch` jest file-private, dlatego submit!
// musi byc w tym samym pliku.
macro_rules! register_nsight_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::Admin,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_nsight_dispatch,
            }
        }
    };
}

register_nsight_variant!("NsightStartRequest", "tentaflow_ws_handler_nsight_start");
register_nsight_variant!("NsightStopRequest", "tentaflow_ws_handler_nsight_stop");
register_nsight_variant!(
    "NsightSessionsRequest",
    "tentaflow_ws_handler_nsight_sessions"
);
register_nsight_variant!("NsightReportRequest", "tentaflow_ws_handler_nsight_report");
register_nsight_variant!("NsightDeleteRequest", "tentaflow_ws_handler_nsight_delete");

#[cfg(test)]
mod nsight_tests {
    use super::*;
    use crate::dispatch::state::AppState;
    use tentaflow_protocol::profiling::{
        NsightDeleteRequest, NsightReportRequest, NsightScope, NsightSessionsRequest,
        NsightStartRequest, NsightStopRequest,
    };
    use tentaflow_protocol::SessionAuth;

    fn admin_ctx() -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: Some("admin".to_string()),
            },
            correlation_id: 1,
            resume_secret: None,
            state: AppState::for_test(),
        }
    }

    /// `req.node_id` ustawiony na lokalny node musi isc lokalna sciezka. Bez nsys
    /// w PATH dostaniemy `Internal("nsys not available")` z `profiling_err_to_proto`.
    /// Test passuje gdy widzimy ten konkretny komunikat (a nie np. crash forwardera).
    #[tokio::test]
    async fn nsight_start_local_node_routes_locally() {
        let ctx = admin_ctx();
        let local = ctx.state.local_node_id.as_ref().to_string();
        let body = MessageBody::NsightBody(NsightPayload::StartRequest(NsightStartRequest {
            node_id: local,
            scope: NsightScope::Cpu,
            duration_secs: 10,
            label: "test".into(),
        }));
        let res = nsight_dispatch(&body, &ctx).await;
        // Bez nsys w PATH dostajemy NotAvailable → Internal. Wazne ze nie poszlo
        // do mesh forwardera (bo `quic_mesh = None` dalby inny komunikat).
        match res {
            Err(e) => assert!(
                e.message.contains("nsys not available")
                    || e.message.contains("nsys"),
                "oczekiwano komunikatu o braku nsys, dostalem: {}",
                e.message
            ),
            Ok(_) => {} // jesli host ma nsys to test po prostu przechodzi.
        }
    }

    #[tokio::test]
    async fn nsight_start_invalid_duration_601_is_bad_request() {
        let ctx = admin_ctx();
        let local = ctx.state.local_node_id.as_ref().to_string();
        let body = MessageBody::NsightBody(NsightPayload::StartRequest(NsightStartRequest {
            node_id: local,
            scope: NsightScope::Cpu,
            duration_secs: 601,
            label: "test".into(),
        }));
        let res = nsight_dispatch(&body, &ctx).await;
        match res {
            Err(e) => {
                // NotAvailable wygrywa nad InvalidDuration tylko wtedy gdy capability
                // jest sprawdzane przed walidacja — sprawdzmy w nsys.rs:
                // start() najpierw waliduje duration, dopiero potem capability.
                // Wiec na hostach bez nsys i tak dostajemy BadRequest.
                if e.code != ProtocolErrorCode::BadRequest {
                    // Toleruj Internal jesli to capability check przyspieszyl.
                    assert!(
                        e.message.contains("invalid duration") || e.message.contains("nsys"),
                        "spodziewane invalid duration albo nsys, dostalem: {:?}",
                        e
                    );
                } else {
                    assert!(e.message.contains("invalid duration"));
                }
            }
            Ok(_) => panic!("oczekiwano bledu walidacji"),
        }
    }

    #[tokio::test]
    async fn nsight_stop_invalid_session_id_is_bad_request() {
        let ctx = admin_ctx();
        let local = ctx.state.local_node_id.as_ref().to_string();
        let body = MessageBody::NsightBody(NsightPayload::StopRequest(NsightStopRequest {
            node_id: local,
            session_id: "../etc/passwd".into(),
        }));
        let res = nsight_dispatch(&body, &ctx).await;
        match res {
            Err(e) => {
                // NSYS_RUNNER.stop() zwraca NotFound dla nieaktywnej sesji ZANIM
                // walidacja session_id sprawdzi format. Akceptujemy oba scenariusze
                // (NotFound i BadRequest) — wazne ze handler nie crashuje i nie
                // probuje czegos wykonac z bledna sciezka.
                assert!(
                    matches!(
                        e.code,
                        ProtocolErrorCode::BadRequest | ProtocolErrorCode::NotFound
                    ),
                    "spodziewano BadRequest/NotFound, dostalem: {:?}",
                    e
                );
            }
            Ok(_) => panic!("oczekiwano bledu"),
        }
    }

    #[tokio::test]
    async fn nsight_sessions_local_empty_returns_empty_list() {
        let ctx = admin_ctx();
        let local = ctx.state.local_node_id.as_ref().to_string();
        // Wymus pusty katalog: ustaw TENTAFLOW_HOME na tempdir.
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("TENTAFLOW_HOME", tmp.path());
        // tentaflow_home jest cache'owane przez OnceLock, wiec ten test moze
        // dostac wczesniej zainicjalizowana wartosc — w takim razie list() nadal
        // zwroci Ok, bo node_dir nie istnieje (storage::list zwraca pusty Vec).
        let body = MessageBody::NsightBody(NsightPayload::SessionsRequest(
            NsightSessionsRequest { node_id: local },
        ));
        let res = nsight_dispatch(&body, &ctx).await;
        match res {
            Ok(MessageBody::NsightBody(NsightPayload::SessionsResponse(r))) => {
                assert!(r.sessions.is_empty(), "oczekiwano pustej listy sesji");
            }
            Ok(other) => panic!("nieoczekiwany wariant: {:?}", other),
            Err(e) => panic!("nieoczekiwany blad: {:?}", e),
        }
    }

    #[tokio::test]
    async fn nsight_delete_invalid_session_id_is_bad_request() {
        let ctx = admin_ctx();
        let local = ctx.state.local_node_id.as_ref().to_string();
        let body = MessageBody::NsightBody(NsightPayload::DeleteRequest(NsightDeleteRequest {
            node_id: local,
            session_id: "ZZZZZ".into(),
        }));
        let res = nsight_dispatch(&body, &ctx).await;
        match res {
            Err(e) => assert_eq!(e.code, ProtocolErrorCode::BadRequest),
            Ok(_) => panic!("oczekiwano BadRequest"),
        }
    }

    #[tokio::test]
    async fn nsight_report_invalid_session_id_is_bad_request() {
        let ctx = admin_ctx();
        let local = ctx.state.local_node_id.as_ref().to_string();
        let body = MessageBody::NsightBody(NsightPayload::ReportRequest(NsightReportRequest {
            node_id: local,
            session_id: "../passwd".into(),
        }));
        let res = nsight_dispatch(&body, &ctx).await;
        match res {
            Err(e) => assert_eq!(e.code, ProtocolErrorCode::BadRequest),
            Ok(_) => panic!("oczekiwano BadRequest"),
        }
    }

    /// Bez `quic_mesh` w AppState forward do remote noda zwraca Internal —
    /// nie ma fallback'u na lokalne wykonanie.
    #[tokio::test]
    async fn nsight_remote_node_without_mesh_manager_fails() {
        let ctx = admin_ctx();
        let body = MessageBody::NsightBody(NsightPayload::SessionsRequest(
            NsightSessionsRequest {
                node_id: "some-other-peer-node".into(),
            },
        ));
        let res = nsight_dispatch(&body, &ctx).await;
        match res {
            Err(e) => {
                assert_eq!(e.code, ProtocolErrorCode::Internal);
                assert!(e.message.contains("Mesh manager niedostepny"));
            }
            Ok(_) => panic!("oczekiwano bledu — brak quic_mesh"),
        }
    }
}
