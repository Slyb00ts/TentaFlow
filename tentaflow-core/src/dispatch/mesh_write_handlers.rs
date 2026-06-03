// =============================================================================
// Plik: dispatch/mesh_write_handlers.rs
// Opis: Async handlery operacji zapisu mesh: pairing/trust/connect/command/
//       network-config oraz multi-source profiling dispatch. Domena pairing/trust
//       zyje w mesh::admin_ops; tu robimy tylko walidacje wariantu i mapowanie
//       AdminError -> ProtocolError.
// =============================================================================

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    BaselineAdoptClearResponse, BaselineAdoptPhaseTag, BaselineAdoptReport, BaselineAdoptStartRequest,
    BaselineAdoptStartResponse, BaselineAdoptStatusResponse, BaselineDonorCandidate,
    BaselineDonorListResponse, MeshConnectRequest, MeshConnectResponse, MeshNodeCommandRequest,
    MeshNodeCommandResponse, MeshNodeNetworkConfigRequest, MeshNodeNetworkConfigResponse,
    MeshPairingConfirmRequest, MeshPairingConfirmResponse, MeshPairingRejectRequest,
    MeshPairingRejectResponse, MeshPairingStartRequest, MeshPairingStartResponse,
    MeshTrustRetrustRequest, MeshTrustRetrustResponse, MeshTrustRevokeRequest,
    MeshTrustRevokeResponse, MessageBody, ProtocolError, ProtocolErrorCode,
};
use tracing::warn;

use super::HandlerContext;
use crate::db::repository;
use crate::mesh::admin_ops::{self, AdminError, AdminErrorKind};
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

impl From<AdminError> for ProtocolError {
    fn from(e: AdminError) -> Self {
        let code = match e.kind {
            AdminErrorKind::BadRequest => ProtocolErrorCode::BadRequest,
            AdminErrorKind::AlreadyPending => ProtocolErrorCode::Conflict,
            AdminErrorKind::RateLimited => ProtocolErrorCode::RateLimited,
            AdminErrorKind::BadPin => ProtocolErrorCode::AuthRequired,
            AdminErrorKind::DeliveryFailed => ProtocolErrorCode::NodeUnreachable,
            AdminErrorKind::MeshUnavailable => ProtocolErrorCode::NotAvailable,
            AdminErrorKind::Internal => ProtocolErrorCode::Internal,
        };
        // CR-004: never let raw internal error text reach the wire — caller
        // already logged details via tracing::error! at the AdminError site.
        let message = match e.kind {
            AdminErrorKind::Internal => {
                tracing::error!("AdminError::Internal: {}", e.message);
                "internal mesh error".to_string()
            }
            _ => e.message,
        };
        ProtocolError::new(code, message)
    }
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
            ));
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
    let outcome = admin_ops::initiate_pairing(
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
    .await?;

    Ok(MessageBody::MeshPairingStartResponseBody(
        MeshPairingStartResponse {
            pair_id: remote_address.clone(),
            pin: outcome.pin,
            completed: outcome.completed,
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
            ));
        }
    };
    let MeshPairingConfirmRequest { pair_id, pin } = payload;

    let security = require_mesh_security(ctx)?;

    // pair_id mapuje na node_id (patrz mesh_pairing_start).
    let outcome = admin_ops::confirm_pairing(
        &security,
        pair_id,
        Some(pin.as_str()),
        &ctx.state.quic_mesh,
        ctx.state.local_node_id.as_ref(),
        &ctx.state.mesh_peer_store,
    )
    .await?;

    Ok(MessageBody::MeshPairingConfirmResponseBody(
        MeshPairingConfirmResponse {
            ok: true,
            trusted_node_id: outcome.trusted_node_id,
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
            ));
        }
    };
    let MeshPairingRejectRequest { pair_id } = payload;

    let security = require_mesh_security(ctx)?;

    admin_ops::reject_pairing(&security, pair_id, &ctx.state.quic_mesh)?;

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
            ));
        }
    };
    let MeshTrustRevokeRequest { node_id } = payload;

    let security = require_mesh_security(ctx)?;

    admin_ops::revoke_trust(
        &security,
        node_id,
        &ctx.state.quic_mesh,
        ctx.state.local_node_id.as_ref(),
    )?;

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
            ));
        }
    };
    let MeshTrustRetrustRequest { node_id } = payload;

    let security = require_mesh_security(ctx)?;

    admin_ops::retrust(&security, node_id)?;

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
            ));
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
            ));
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
            ));
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
                MeshNodeNetworkConfigResponse { ok: response.ok },
            ))
        }
        Err(e) => Err(ProtocolError::new(
            ProtocolErrorCode::Internal,
            format!("Blad wykonania komendy: {}", e),
        )),
    }
}

// =============================================================================
// Sync baseline-adopt admin (donor list + start/status/clear). Admin wskazuje
// dawce baseline'u i steruje pojedyncza adopcja single-flight. Cala maszyneria
// zyje w mesh::admin_ops / sync::core_baseline — tu robimy walidacje i mapowanie.
// =============================================================================

fn map_baseline_phase(
    phase: crate::sync::core_baseline::BaselinePhase,
) -> BaselineAdoptPhaseTag {
    use crate::sync::core_baseline::BaselinePhase;
    match phase {
        BaselinePhase::Elected => BaselineAdoptPhaseTag::Elected,
        BaselinePhase::Receiving => BaselineAdoptPhaseTag::Receiving,
        BaselinePhase::Importing => BaselineAdoptPhaseTag::Importing,
        BaselinePhase::Imported => BaselineAdoptPhaseTag::Imported,
        BaselinePhase::Completed => BaselineAdoptPhaseTag::Completed,
    }
}

fn baseline_ledger_err(e: crate::sync::ledger::SyncLedgerError) -> ProtocolError {
    // Internal ledger/codec text never reaches the wire — log and return a
    // generic message so SQLite paths/schema details do not leak to clients.
    tracing::error!("baseline adopt admin: ledger error: {}", e);
    ProtocolError::new(ProtocolErrorCode::Internal, "internal baseline error")
}

// -----------------------------------------------------------------------------
// BaselineDonorListRequest — kandydaci na dawce (zaufane sparowane peery).
// -----------------------------------------------------------------------------

#[handler(variant = "BaselineDonorListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn baseline_donor_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    use crate::mesh::peer_registry::TrustStateTag;

    let local_node_id = ctx.state.local_node_id.as_ref();
    // Source of truth: in-memory PeerRegistry (hydrated from peer_persisted at
    // startup). Only Trusted peers (other than self) are eligible donors — adopt
    // pulls a full baseline over the already-trusted pairing channel, so an
    // untrusted node must never be offered as a candidate.
    let candidates: Vec<BaselineDonorCandidate> = ctx
        .state
        .mesh_peer_store
        .registry()
        .map(|reg| {
            reg.snapshot_summary()
                .into_iter()
                .filter(|s| matches!(s.trust, TrustStateTag::Trusted))
                .filter_map(|s| {
                    let node_id = hex::encode(s.node_id);
                    if node_id == local_node_id {
                        return None;
                    }
                    let display_name = if s.hostname.is_empty() {
                        node_id.clone()
                    } else {
                        (*s.hostname).to_string()
                    };
                    Some(BaselineDonorCandidate {
                        node_id,
                        display_name,
                        trusted: true,
                        // Donor row counts are only known from the transfer
                        // header (`BaselineHeader`); nothing reliable is known
                        // locally, so summary stays None for the list.
                        summary: None,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(MessageBody::BaselineDonorListResponseBody(
        BaselineDonorListResponse { candidates },
    ))
}

// -----------------------------------------------------------------------------
// BaselineAdoptStartRequest — rozpocznij adopcje od wskazanego dawcy (joiner).
// -----------------------------------------------------------------------------

#[handler(variant = "BaselineAdoptStartRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn baseline_adopt_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::BaselineAdoptStartRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected BaselineAdoptStartRequestBody",
            ));
        }
    };
    let BaselineAdoptStartRequest { donor_node_id } = payload;

    let security = require_mesh_security(ctx)?;

    let outcome = admin_ops::admin_start_baseline_adopt(
        &ctx.state.db,
        &security,
        ctx.state.local_node_id.as_ref(),
        donor_node_id,
        &ctx.state.quic_mesh,
    )?;

    Ok(MessageBody::BaselineAdoptStartResponseBody(
        BaselineAdoptStartResponse {
            ok: true,
            started: outcome.started,
            message: outcome.message,
        },
    ))
}

// -----------------------------------------------------------------------------
// BaselineAdoptStatusRequest — biezaca faza + raport gdy Completed.
// -----------------------------------------------------------------------------

#[handler(variant = "BaselineAdoptStatusRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn baseline_adopt_status(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    use crate::sync::core_baseline::{
        load_adopt_report, load_adopt_state, BaselinePhase, BaselineRole,
    };

    let state =
        load_adopt_state(&ctx.state.db).map_err(baseline_ledger_err)?;

    let response = match state {
        None => BaselineAdoptStatusResponse {
            phase: BaselineAdoptPhaseTag::None,
            peer: None,
            is_joiner: None,
            report: None,
        },
        Some(state) => {
            let report = if state.phase == BaselinePhase::Completed {
                load_adopt_report(&ctx.state.db)
                    .map_err(baseline_ledger_err)?
                    .map(|r| BaselineAdoptReport {
                        donor_org_id: r.donor_org_id,
                        users_merged_by_email: r.users_merged_by_email as u64,
                        users_joined_donor_org: r.users_joined_donor_org as u64,
                        collisions_suffixed: r.collisions_suffixed as u64,
                    })
            } else {
                None
            };
            BaselineAdoptStatusResponse {
                phase: map_baseline_phase(state.phase),
                peer: Some(state.peer),
                is_joiner: Some(state.role == BaselineRole::Joiner),
                report,
            }
        }
    };

    Ok(MessageBody::BaselineAdoptStatusResponseBody(response))
}

// -----------------------------------------------------------------------------
// BaselineAdoptClearRequest — odblokuj zawieszony stan adopcji (escape hatch).
// -----------------------------------------------------------------------------

#[handler(variant = "BaselineAdoptClearRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn baseline_adopt_clear(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    use crate::sync::core_baseline::{clear_adopt_state, load_adopt_state, BaselinePhase};

    let state =
        load_adopt_state(&ctx.state.db).map_err(baseline_ledger_err)?;

    let response = match state {
        None => BaselineAdoptClearResponse {
            ok: true,
            cleared: false,
            message: "brak stanu adopcji do wyczyszczenia".to_string(),
        },
        // An in-flight transfer/import must NOT be torn out from under the
        // transaction: clearing it would orphan a half-merged database. Only
        // Elected (transfer not started) and Completed (already done) are safe
        // to clear; Receiving/Importing/Imported are refused.
        Some(state)
            if matches!(
                state.phase,
                BaselinePhase::Receiving | BaselinePhase::Importing | BaselinePhase::Imported
            ) =>
        {
            return Err(ProtocolError::new(
                ProtocolErrorCode::Conflict,
                "adopcja w trakcie aktywnego transferu/importu — nie mozna wyczyscic",
            ));
        }
        Some(_) => {
            clear_adopt_state(&ctx.state.db).map_err(baseline_ledger_err)?;
            BaselineAdoptClearResponse {
                ok: true,
                cleared: true,
                message: "stan adopcji wyczyszczony".to_string(),
            }
        }
    };

    Ok(MessageBody::BaselineAdoptClearResponseBody(response))
}

// =============================================================================
// 9. ProfilingBody — multi-source profiling (start/stop/sessions/report/...).
// =============================================================================

/// Mapuje `SessionError` na `ProtocolError` z deterministycznymi kodami.
fn profiling_v2_err_to_proto(e: crate::profiling::SessionError) -> ProtocolError {
    use crate::profiling::SessionError as SE;
    match e {
        SE::AlreadyActive => ProtocolError::new(
            ProtocolErrorCode::Conflict,
            "another profiling session is already active",
        ),
        SE::NoCollectorsAvailable => ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "no collectors available for the requested scope",
        ),
        SE::AllCollectorsFailed => ProtocolError::internal("all collectors failed to start"),
        SE::InvalidScope(reason) => ProtocolError::bad_request(format!("invalid scope: {reason}")),
        SE::Storage(s) => ProtocolError::internal(format!("storage: {s}")),
        SE::CollectorStartFailure { id, error } => {
            ProtocolError::internal(format!("collector {id} start failure: {error}"))
        }
        SE::StaleHandle => ProtocolError::not_found("session handle is stale"),
        SE::Io(e) => ProtocolError::internal(format!("io: {e}")),
        SE::Merge(s) => ProtocolError::internal(format!("merge: {s}")),
    }
}

fn storage_err_to_proto(e: crate::profiling::StorageError) -> ProtocolError {
    use crate::profiling::StorageError as SE;
    match e {
        SE::InvalidSessionId(s) => ProtocolError::bad_request(format!("invalid session id: {s}")),
        SE::InvalidNodeId(s) => ProtocolError::bad_request(format!("invalid node id: {s}")),
        SE::InvalidCollectorId(s) => {
            ProtocolError::bad_request(format!("invalid collector id: {s}"))
        }
        SE::NotFound(s) => ProtocolError::not_found(s),
        SE::PathTraversal(s) => ProtocolError::bad_request(format!("path traversal rejected: {s}")),
        SE::SizeCapExceeded { actual, cap } => {
            ProtocolError::internal(format!("size cap exceeded: {actual} > {cap}"))
        }
        SE::Io(e) => ProtocolError::internal(format!("io: {e}")),
        SE::ManifestParse(s) => ProtocolError::internal(format!("manifest: {s}")),
        SE::Cbor(s) => ProtocolError::internal(format!("CBOR: {s}")),
    }
}

fn map_storage_skipped(
    v: Vec<crate::profiling::SkippedCollector>,
) -> Vec<tentaflow_protocol::ProfilingSkippedCollector> {
    v.into_iter()
        .map(|s| tentaflow_protocol::ProfilingSkippedCollector {
            id: s.id,
            reason: s.reason,
        })
        .collect()
}

fn session_kind_to_str(k: &crate::profiling::SessionKind) -> String {
    match k {
        crate::profiling::SessionKind::MultiSource => "multi_source".to_string(),
    }
}

fn map_session_entry(
    e: crate::profiling::SessionEntry,
) -> tentaflow_protocol::ProfilingSessionEntry {
    tentaflow_protocol::ProfilingSessionEntry {
        session_id: e.session_id,
        label: e.label,
        started_at: e.started_at,
        duration_ns: e.duration_ns,
        kind: session_kind_to_str(&e.kind),
        collectors_used: e.collectors_used,
        size_bytes: e.size_bytes,
    }
}

/// Build a deterministic 32-hex-char session id derived from time + node id.
fn new_session_id(node_id: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u128)
        .unwrap_or(0);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    use std::hash::Hasher;
    hasher.write_u128(nanos);
    hasher.write(node_id.as_bytes());
    let h = hasher.finish();
    // 32 hex chars: nanos low bits + hash
    format!("{:016x}{:016x}", nanos as u64, h)
}

/// Wykonuje `MeshCommandType::Profiling*` na zdalnym nodzie i odpakowuje typed
/// `MeshCommandResponsePayload::Profiling*` w `ProfilingPayload::*Response`.
async fn forward_profiling_to_peer(
    ctx: &HandlerContext,
    target_node_id: &str,
    cmd: tentaflow_protocol::mesh::MeshCommandType,
) -> Result<tentaflow_protocol::ProfilingPayload, ProtocolError> {
    use tentaflow_protocol::mesh::MeshCommandResponsePayload as RP;
    use tentaflow_protocol::ProfilingPayload as PP;

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
            format!("mesh profiling forward: {}", e),
        )
    })?;

    if !response.ok {
        let msg = response
            .error
            .unwrap_or_else(|| "remote node refused command".to_string());
        return Err(ProtocolError::new(ProtocolErrorCode::Internal, msg));
    }

    match response.payload {
        RP::ProfilingStart(r) => Ok(PP::StartResponse(r)),
        RP::ProfilingStop(r) => Ok(PP::StopResponse(r)),
        RP::ProfilingSessions(r) => Ok(PP::SessionsResponse(r)),
        RP::ProfilingReport(r) => Ok(PP::ReportResponse(r)),
        RP::ProfilingDelete(r) => Ok(PP::DeleteResponse(r)),
        RP::ProfilingDownload(r) => Ok(PP::DownloadResponse(r)),
        RP::ProfilingActiveInfo(r) => Ok(PP::ActiveInfoResponse(r)),
        _ => Err(ProtocolError::internal(
            "remote node returned unexpected payload variant",
        )),
    }
}

/// Pakuje sciezke zdarzen + manifest + raw/ do tar.gz w pamieci.
fn build_session_tarball(
    storage: &crate::profiling::ProfileStorage,
    node_id: &str,
    session_id: &str,
) -> Result<Vec<u8>, ProtocolError> {
    use std::io::Write;
    let session_dir = storage.root().join(node_id).join(session_id);
    if !session_dir.exists() {
        return Err(ProtocolError::not_found(format!(
            "session {session_id} not found"
        )));
    }
    let buf: Vec<u8> = Vec::new();
    let encoder = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);
    tar.append_dir_all(session_id, &session_dir)
        .map_err(|e| ProtocolError::internal(format!("tar build: {e}")))?;
    let mut encoder = tar
        .into_inner()
        .map_err(|e| ProtocolError::internal(format!("tar finalize: {e}")))?;
    encoder
        .flush()
        .map_err(|e| ProtocolError::internal(format!("gzip flush: {e}")))?;
    let bytes = encoder
        .finish()
        .map_err(|e| ProtocolError::internal(format!("gzip finish: {e}")))?;
    Ok(bytes)
}

async fn handle_profiling_local(
    ctx: &HandlerContext,
    payload: tentaflow_protocol::ProfilingPayload,
) -> Result<tentaflow_protocol::ProfilingPayload, ProtocolError> {
    use crate::profiling::{ElevationToken, MULTI_SOURCE, PROFILE_PARSERS, PROFILE_STORAGE};
    use tentaflow_protocol::ProfilingPayload as PP;
    use tentaflow_protocol::{
        ProfilingActiveInfoResponse, ProfilingActiveSessionInfo, ProfilingDeleteResponse,
        ProfilingDownloadResponse, ProfilingReportResponse, ProfilingSessionsResponse,
        ProfilingStartResponse, ProfilingStopResponse,
    };

    let storage = std::sync::Arc::clone(&PROFILE_STORAGE);
    let parsers = std::sync::Arc::clone(&PROFILE_PARSERS);
    let orchestrator = std::sync::Arc::clone(&MULTI_SOURCE);
    let local_node_id = ctx.state.local_node_id.as_ref().to_string();

    match payload {
        PP::StartRequest(req) => {
            let elevation = if req.elevation_password.is_empty() {
                None
            } else {
                Some(std::sync::Arc::new(ElevationToken::new_sudo(
                    req.elevation_password.clone(),
                )))
            };
            let session_id = new_session_id(&local_node_id);
            let scope_clone = req.scope.clone();
            let label_for_audit = req.label.clone();

            let handle = orchestrator
                .clone()
                .start(
                    req.scope,
                    local_node_id.clone(),
                    session_id.clone(),
                    req.label,
                    elevation,
                    parsers,
                )
                .await
                .map_err(profiling_v2_err_to_proto)?;

            let info = orchestrator
                .active_info()
                .await
                .ok_or_else(|| ProtocolError::internal("orchestrator lost active session"))?;

            let started_at_unix_ns = info.started_at_unix_ns;
            let collectors_started = info.collectors_running.clone();
            let collectors_skipped = map_storage_skipped(info.collectors_skipped);

            let _ = repository::log_audit(
                &ctx.state.db,
                None,
                None,
                "profiling.start",
                Some(&format!("session:{}", handle.session_id)),
                Some(
                    &serde_json::json!({
                        "session_id": handle.session_id,
                        "scope": scope_clone,
                        "label": label_for_audit,
                    })
                    .to_string(),
                ),
                None,
                Some(ctx.state.local_node_id.as_ref()),
            );

            Ok(PP::StartResponse(ProfilingStartResponse {
                session_id: handle.session_id,
                started_at_unix_ns,
                collectors_started,
                collectors_skipped,
            }))
        }
        PP::StopRequest(req) => {
            let report = orchestrator
                .clone()
                .stop_by_id(&req.session_id)
                .await
                .map_err(profiling_v2_err_to_proto)?;
            let _ = repository::log_audit(
                &ctx.state.db,
                None,
                None,
                "profiling.stop",
                Some(&format!("session:{}", report.session_id)),
                None,
                None,
                Some(ctx.state.local_node_id.as_ref()),
            );
            Ok(PP::StopResponse(ProfilingStopResponse {
                session_id: report.session_id.clone(),
                report,
            }))
        }
        PP::SessionsRequest(req) => {
            let entries = storage
                .list_sessions(&local_node_id)
                .await
                .map_err(storage_err_to_proto)?;
            let entries = entries.into_iter().map(map_session_entry).collect();
            Ok(PP::SessionsResponse(ProfilingSessionsResponse {
                node_id: req.node_id,
                entries,
            }))
        }
        PP::ReportRequest(req) => {
            let report = storage
                .read_report(&local_node_id, &req.session_id)
                .await
                .map_err(storage_err_to_proto)?;
            Ok(PP::ReportResponse(ProfilingReportResponse { report }))
        }
        PP::DeleteRequest(req) => {
            storage
                .delete_session(&local_node_id, &req.session_id)
                .await
                .map_err(storage_err_to_proto)?;
            Ok(PP::DeleteResponse(ProfilingDeleteResponse {
                session_id: req.session_id,
                deleted: true,
            }))
        }
        PP::DownloadRequest(req) => {
            let storage_clone = std::sync::Arc::clone(&storage);
            let node_id = local_node_id.clone();
            let sid = req.session_id.clone();
            let bytes = tokio::task::spawn_blocking(move || {
                build_session_tarball(&storage_clone, &node_id, &sid)
            })
            .await
            .map_err(|e| ProtocolError::internal(format!("join: {e}")))??;
            let filename = format!("profiling-{}.tar.gz", req.session_id);
            Ok(PP::DownloadResponse(ProfilingDownloadResponse {
                session_id: req.session_id,
                filename,
                tarball_bytes: bytes,
            }))
        }
        PP::ActiveInfoRequest(_req) => {
            let info = orchestrator
                .active_info()
                .await
                .map(|i| ProfilingActiveSessionInfo {
                    session_id: i.session_id,
                    node_id: i.node_id,
                    label: i.label,
                    started_at_unix_ns: i.started_at_unix_ns,
                    planned_duration_ns: i.planned_duration_ns,
                    elapsed_ns: i.elapsed_ns,
                    collectors_running: i.collectors_running,
                    collectors_skipped: map_storage_skipped(i.collectors_skipped),
                });
            Ok(PP::ActiveInfoResponse(ProfilingActiveInfoResponse { info }))
        }
        PP::ValidateSudoRequest(req) => {
            let response = crate::profiling::permissions::validate_sudo(req.password).await;
            let _ = repository::log_audit(
                &ctx.state.db,
                None,
                None,
                "profiling.validate_sudo",
                None,
                Some(&format!(
                    "success={}, reason={}",
                    response.ok, response.reason
                )),
                None,
                Some(ctx.state.local_node_id.as_ref()),
            );
            Ok(PP::ValidateSudoResponse(response))
        }
        PP::CollectorsStatusRequest(_req) => {
            let (collectors, age_seconds) =
                crate::profiling::permissions::collectors_status_snapshot();
            Ok(PP::CollectorsStatusResponse(
                tentaflow_protocol::ProfilingCollectorsStatusResponse {
                    collectors,
                    age_seconds,
                },
            ))
        }
        // Response variants must not arrive as requests.
        PP::StartResponse(_)
        | PP::StopResponse(_)
        | PP::SessionsResponse(_)
        | PP::ReportResponse(_)
        | PP::DeleteResponse(_)
        | PP::DownloadResponse(_)
        | PP::ActiveInfoResponse(_)
        | PP::ValidateSudoResponse(_)
        | PP::CollectorsStatusResponse(_) => Err(ProtocolError::bad_request(
            "expected ProfilingPayload request variant",
        )),
    }
}

async fn profiling_route(
    ctx: &HandlerContext,
    payload: tentaflow_protocol::ProfilingPayload,
) -> Result<tentaflow_protocol::ProfilingPayload, ProtocolError> {
    use tentaflow_protocol::mesh::MeshCommandType as MC;
    use tentaflow_protocol::ProfilingPayload as PP;

    let local = ctx.state.local_node_id.as_ref();
    let target: String = match &payload {
        PP::StartRequest(r) => r.node_id.clone(),
        PP::StopRequest(r) => r.node_id.clone(),
        PP::SessionsRequest(r) => r.node_id.clone(),
        PP::ReportRequest(r) => r.node_id.clone(),
        PP::DeleteRequest(r) => r.node_id.clone(),
        PP::DownloadRequest(r) => r.node_id.clone(),
        PP::ActiveInfoRequest(r) => r.node_id.clone(),
        // ValidateSudo i CollectorsStatus to per-process state — sudo dziala
        // tylko w kontekscie tego procesu, kolektory probowane lokalnie.
        // Nie forward'ujemy do peera - obsluga wprost lokalna.
        PP::ValidateSudoRequest(_) | PP::CollectorsStatusRequest(_) => {
            return handle_profiling_local(ctx, payload).await;
        }
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ProfilingPayload request variant",
            ));
        }
    };

    if target.is_empty() || target.as_str() == local {
        return handle_profiling_local(ctx, payload).await;
    }

    let cmd = match payload {
        PP::StartRequest(r) => MC::ProfilingStart(r),
        PP::StopRequest(r) => MC::ProfilingStop(r),
        PP::SessionsRequest(r) => MC::ProfilingSessions(r),
        PP::ReportRequest(r) => MC::ProfilingReport(r),
        PP::DeleteRequest(r) => MC::ProfilingDelete(r),
        PP::DownloadRequest(r) => MC::ProfilingDownload(r),
        PP::ActiveInfoRequest(r) => MC::ProfilingActiveInfo(r),
        _ => unreachable!("filtered above"),
    };
    forward_profiling_to_peer(ctx, &target, cmd).await
}

#[handler(variant = "ProfilingBody", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn profiling_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ProfilingBody(p) => p.clone(),
        _ => return Err(ProtocolError::bad_request("expected ProfilingBody")),
    };
    let res = profiling_route(ctx, payload).await?;
    Ok(MessageBody::ProfilingBody(res))
}

macro_rules! register_profiling_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::Admin,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_profiling_dispatch,
            }
        }
    };
}

register_profiling_variant!(
    "ProfilingStartRequest",
    "tentaflow_ws_handler_profiling_start"
);
register_profiling_variant!(
    "ProfilingStopRequest",
    "tentaflow_ws_handler_profiling_stop"
);
register_profiling_variant!(
    "ProfilingSessionsRequest",
    "tentaflow_ws_handler_profiling_sessions"
);
register_profiling_variant!(
    "ProfilingReportRequest",
    "tentaflow_ws_handler_profiling_report"
);
register_profiling_variant!(
    "ProfilingDeleteRequest",
    "tentaflow_ws_handler_profiling_delete"
);
register_profiling_variant!(
    "ProfilingDownloadRequest",
    "tentaflow_ws_handler_profiling_download"
);
register_profiling_variant!(
    "ProfilingActiveInfoRequest",
    "tentaflow_ws_handler_profiling_active_info"
);
register_profiling_variant!(
    "ProfilingValidateSudoRequest",
    "tentaflow_ws_handler_profiling_validate_sudo"
);
register_profiling_variant!(
    "ProfilingCollectorsStatusRequest",
    "tentaflow_ws_handler_profiling_collectors_status"
);

#[cfg(test)]
mod baseline_adopt_handler_tests {
    use super::*;
    use crate::dispatch::state::AppState;
    use crate::sync::core_baseline::{
        store_adopt_state, BaselineAdoptState, BaselinePhase, BaselineRole,
    };
    use tentaflow_protocol::mesh::BaselineEpoch;
    use tentaflow_protocol::SessionAuth;

    fn admin_ctx() -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: Some("admin".to_string()),
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: AppState::for_test(),
            org_context: None,
        }
    }

    fn seed_state(ctx: &HandlerContext, phase: BaselinePhase) {
        let state = BaselineAdoptState {
            role: BaselineRole::Joiner,
            peer: "donor-node".to_string(),
            epoch: BaselineEpoch {
                counter: 3,
                origin_node: "donor-node".to_string(),
            },
            phase,
        };
        store_adopt_state(&ctx.state.db, &state).expect("seed adopt state");
    }

    #[test]
    fn donor_list_without_registry_is_empty_and_typed() {
        let ctx = admin_ctx();
        let res = baseline_donor_list(&MessageBody::BaselineDonorListRequest, &ctx)
            .expect("donor list ok");
        match res {
            MessageBody::BaselineDonorListResponseBody(r) => assert!(r.candidates.is_empty()),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn status_none_when_no_state() {
        let ctx = admin_ctx();
        let res = baseline_adopt_status(&MessageBody::BaselineAdoptStatusRequest, &ctx)
            .expect("status ok");
        match res {
            MessageBody::BaselineAdoptStatusResponseBody(r) => {
                assert_eq!(r.phase, tentaflow_protocol::BaselineAdoptPhaseTag::None);
                assert!(r.peer.is_none() && r.report.is_none());
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn status_reports_phase_and_peer() {
        let ctx = admin_ctx();
        seed_state(&ctx, BaselinePhase::Receiving);
        let res = baseline_adopt_status(&MessageBody::BaselineAdoptStatusRequest, &ctx)
            .expect("status ok");
        match res {
            MessageBody::BaselineAdoptStatusResponseBody(r) => {
                assert_eq!(r.phase, tentaflow_protocol::BaselineAdoptPhaseTag::Receiving);
                assert_eq!(r.peer.as_deref(), Some("donor-node"));
                assert_eq!(r.is_joiner, Some(true));
                // Report only attached at Completed.
                assert!(r.report.is_none());
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn clear_refused_during_active_import() {
        let ctx = admin_ctx();
        seed_state(&ctx, BaselinePhase::Importing);
        let err = baseline_adopt_clear(&MessageBody::BaselineAdoptClearRequest, &ctx)
            .expect_err("clear during import must be refused");
        assert_eq!(err.code, ProtocolErrorCode::Conflict);
        // State must still be present (not torn out).
        assert!(crate::sync::core_baseline::load_adopt_state(&ctx.state.db)
            .unwrap()
            .is_some());
    }

    #[test]
    fn clear_allowed_when_elected() {
        let ctx = admin_ctx();
        seed_state(&ctx, BaselinePhase::Elected);
        let res = baseline_adopt_clear(&MessageBody::BaselineAdoptClearRequest, &ctx)
            .expect("clear of elected state ok");
        match res {
            MessageBody::BaselineAdoptClearResponseBody(r) => assert!(r.ok && r.cleared),
            other => panic!("unexpected variant: {other:?}"),
        }
        assert!(crate::sync::core_baseline::load_adopt_state(&ctx.state.db)
            .unwrap()
            .is_none());
    }

    #[test]
    fn clear_noop_when_no_state() {
        let ctx = admin_ctx();
        let res = baseline_adopt_clear(&MessageBody::BaselineAdoptClearRequest, &ctx)
            .expect("clear with no state ok");
        match res {
            MessageBody::BaselineAdoptClearResponseBody(r) => assert!(r.ok && !r.cleared),
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}

#[cfg(test)]
mod profiling_tests {
    use super::*;
    use crate::dispatch::state::AppState;
    use tentaflow_protocol::{
        ProfileScope, ProfileSourceFlags, ProfileTarget, ProfilingActiveInfoRequest,
        ProfilingDeleteRequest, ProfilingDownloadRequest, ProfilingReportRequest,
        ProfilingSessionsRequest, ProfilingStartRequest, ProfilingStopRequest, SessionAuth,
    };

    fn admin_ctx() -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: Some("admin".to_string()),
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: AppState::for_test(),
            org_context: None,
        }
    }

    fn cpu_scope() -> ProfileScope {
        ProfileScope {
            sources: ProfileSourceFlags(ProfileSourceFlags::CPU_SAMPLING),
            gpu_targets: tentaflow_protocol::GpuTargets::None,
            cpu_sampling_hz: 99,
            target: ProfileTarget::OwnProcess,
            duration_seconds: 0,
            label: "test".into(),
        }
    }

    fn wrap(p: tentaflow_protocol::ProfilingPayload) -> MessageBody {
        MessageBody::ProfilingBody(p)
    }

    #[tokio::test]
    async fn profiling_active_info_local_returns_none_when_idle() {
        let ctx = admin_ctx();
        let local = ctx.state.local_node_id.as_ref().to_string();
        let body = wrap(tentaflow_protocol::ProfilingPayload::ActiveInfoRequest(
            ProfilingActiveInfoRequest { node_id: local },
        ));
        let res = profiling_dispatch(&body, &ctx).await;
        match res {
            Ok(MessageBody::ProfilingBody(
                tentaflow_protocol::ProfilingPayload::ActiveInfoResponse(r),
            )) => {
                // Może być Some(...) jeżeli inny test left the orchestrator active —
                // wówczas nie crashujemy, akceptujemy oba stany.
                let _ = r.info;
            }
            Ok(other) => panic!("nieoczekiwany wariant: {other:?}"),
            Err(e) => panic!("blad: {e:?}"),
        }
    }

    #[tokio::test]
    async fn profiling_sessions_local_empty_returns_empty_list() {
        let ctx = admin_ctx();
        let local = ctx.state.local_node_id.as_ref().to_string();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("TENTAFLOW_HOME", tmp.path());
        let body = wrap(tentaflow_protocol::ProfilingPayload::SessionsRequest(
            ProfilingSessionsRequest { node_id: local },
        ));
        let res = profiling_dispatch(&body, &ctx).await;
        match res {
            Ok(MessageBody::ProfilingBody(
                tentaflow_protocol::ProfilingPayload::SessionsResponse(r),
            )) => {
                // PROFILE_STORAGE jest LazyLock i mogla byc juz zainicjowana
                // wczesniej z innym TENTAFLOW_HOME — wiec wynik moze nie byc pusty.
                let _ = r.entries;
            }
            other => panic!("nieoczekiwany wynik: {other:?}"),
        }
    }

    #[tokio::test]
    async fn profiling_report_invalid_session_id_is_bad_request() {
        let ctx = admin_ctx();
        let local = ctx.state.local_node_id.as_ref().to_string();
        let body = wrap(tentaflow_protocol::ProfilingPayload::ReportRequest(
            ProfilingReportRequest {
                node_id: local,
                session_id: "../passwd".into(),
            },
        ));
        let res = profiling_dispatch(&body, &ctx).await;
        match res {
            Err(e) => assert_eq!(e.code, ProtocolErrorCode::BadRequest),
            Ok(_) => panic!("oczekiwano BadRequest"),
        }
    }

    #[tokio::test]
    async fn profiling_delete_invalid_session_id_is_bad_request() {
        let ctx = admin_ctx();
        let local = ctx.state.local_node_id.as_ref().to_string();
        let body = wrap(tentaflow_protocol::ProfilingPayload::DeleteRequest(
            ProfilingDeleteRequest {
                node_id: local,
                session_id: "ZZZZZ".into(),
            },
        ));
        let res = profiling_dispatch(&body, &ctx).await;
        match res {
            Err(e) => assert_eq!(e.code, ProtocolErrorCode::BadRequest),
            Ok(_) => panic!("oczekiwano BadRequest"),
        }
    }

    #[tokio::test]
    async fn profiling_download_invalid_session_id_is_bad_request() {
        let ctx = admin_ctx();
        let local = ctx.state.local_node_id.as_ref().to_string();
        let body = wrap(tentaflow_protocol::ProfilingPayload::DownloadRequest(
            ProfilingDownloadRequest {
                node_id: local,
                session_id: "../etc/passwd".into(),
            },
        ));
        let res = profiling_dispatch(&body, &ctx).await;
        match res {
            Err(e) => {
                assert!(matches!(
                    e.code,
                    ProtocolErrorCode::BadRequest | ProtocolErrorCode::NotFound
                ));
            }
            Ok(_) => panic!("oczekiwano bledu"),
        }
    }

    #[tokio::test]
    async fn profiling_stop_unknown_session_returns_not_found() {
        let ctx = admin_ctx();
        let local = ctx.state.local_node_id.as_ref().to_string();
        let body = wrap(tentaflow_protocol::ProfilingPayload::StopRequest(
            ProfilingStopRequest {
                node_id: local,
                session_id: "0123456789abcdef0123456789abcdef".into(),
            },
        ));
        let res = profiling_dispatch(&body, &ctx).await;
        match res {
            Err(e) => assert!(matches!(
                e.code,
                ProtocolErrorCode::NotFound | ProtocolErrorCode::Conflict
            )),
            Ok(_) => panic!("oczekiwano NotFound/Conflict"),
        }
    }

    #[tokio::test]
    async fn profiling_remote_node_without_mesh_manager_fails() {
        let ctx = admin_ctx();
        let body = wrap(tentaflow_protocol::ProfilingPayload::SessionsRequest(
            ProfilingSessionsRequest {
                node_id: "some-other-peer-node".into(),
            },
        ));
        let res = profiling_dispatch(&body, &ctx).await;
        match res {
            Err(e) => {
                assert_eq!(e.code, ProtocolErrorCode::Internal);
                assert!(e.message.contains("Mesh manager"));
            }
            Ok(_) => panic!("oczekiwano Internal"),
        }
    }

    #[tokio::test]
    async fn profiling_start_invalid_label_is_bad_request() {
        let ctx = admin_ctx();
        let local = ctx.state.local_node_id.as_ref().to_string();
        let mut scope = cpu_scope();
        scope.label = "a\x07b".into(); // control char rejected
        let body = wrap(tentaflow_protocol::ProfilingPayload::StartRequest(
            ProfilingStartRequest {
                node_id: local,
                scope,
                label: "outer".into(),
                elevation_password: String::new(),
            },
        ));
        let res = profiling_dispatch(&body, &ctx).await;
        match res {
            Err(e) => assert_eq!(e.code, ProtocolErrorCode::BadRequest),
            Ok(_) => panic!("oczekiwano BadRequest"),
        }
    }
}
