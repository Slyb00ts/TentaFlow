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
    ProtocolError, ProtocolErrorCode,
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
