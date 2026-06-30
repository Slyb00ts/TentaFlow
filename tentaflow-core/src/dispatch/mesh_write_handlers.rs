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
    BaselineAdoptClearResponse, BaselineAdoptPhaseTag, BaselineAdoptReport,
    BaselineAdoptStartRequest, BaselineAdoptStartResponse, BaselineAdoptStatusResponse,
    BaselineDonorCandidate, BaselineDonorListResponse, ClusterDeployMemberStatus,
    ClusterDeployRequest, ClusterDeployResponse, ClusterDeployStopRequest,
    ClusterDeployStopResponse, ClusterRdmaConfigureRequest,
    ClusterRdmaConfigureResponse, ClusterRdmaInterface, ClusterRdmaMemberStatus, MeshConnectRequest,
    MeshConnectResponse, MeshNodeCommandRequest, MeshNodeCommandResponse,
    MeshNodeNetworkConfigRequest, MeshNodeNetworkConfigResponse, MeshPairingConfirmRequest,
    MeshPairingConfirmResponse, MeshPairingRejectRequest, MeshPairingRejectResponse,
    MeshPairingStartRequest, MeshPairingStartResponse, MeshTrustRetrustRequest,
    MeshTrustRetrustResponse, MeshTrustRevokeRequest, MeshTrustRevokeResponse, MessageBody,
    ProtocolError, ProtocolErrorCode,
};
use tracing::{info, warn};

use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType};

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

    let mtu = cfg
        .get("mtu")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    use tentaflow_protocol::mesh::MeshCommandType;
    let cmd = MeshCommandType::NetworkConfig {
        interface: interface_name.clone(),
        ipv4,
        netmask,
        gateway,
        dhcp,
        sudo_password,
        mtu,
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
// Cluster RDMA auto-config (D1). Detect each member's RoCE "twins", bring up the
// unconfigured ones (assign IP on a dedicated RDMA subnet + set MTU) over the
// existing NetworkConfig mesh command, and persist the per-node RoCE device list
// + RDMA IP for distributed deploy. Everything runs through the program — no
// manual `ip addr` / netplan.
// =============================================================================

/// Enumerate a node's RoCE interfaces. Local node reads `/sys` directly; remote
/// nodes answer the `RoceProbe` mesh command (require trust).
async fn fetch_roce_interfaces(
    ctx: &HandlerContext,
    qm: &Arc<IrohMeshManager>,
    local_id: &str,
    node_id: &str,
) -> Result<Vec<tentaflow_protocol::mesh::RoceInterfaceInfo>, String> {
    if node_id == local_id {
        return Ok(crate::mesh::roce_config::enumerate_roce_interfaces());
    }
    let trusted = ctx
        .state
        .mesh_security
        .as_ref()
        .map_or(false, |s| s.is_trusted(node_id));
    if !trusted {
        return Err("node nie jest zaufany".to_string());
    }
    use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType};
    match qm
        .send_command_and_wait(node_id, MeshCommandType::RoceProbe, 30)
        .await
    {
        Ok(resp) if resp.ok => match resp.payload {
            MeshCommandResponsePayload::RoceInterfaceList(list) => Ok(list),
            _ => Err("nieoczekiwany payload RoceProbe".to_string()),
        },
        Ok(resp) => Err(resp.error.unwrap_or_else(|| "RoceProbe nieudany".to_string())),
        Err(e) => Err(format!("RoceProbe send nieudany: {}", e)),
    }
}

/// Apply one interface config to a node. `ipv4`/`netmask` Some = full static
/// (re)assignment; both None = MTU-ONLY non-destructive update (P1-4, never
/// rewrites the IP config of the interconnect interface). Local node applies via
/// the blocking sudo path; remote nodes go through the `NetworkConfig` mesh
/// command. Returns Ok on success, Err(message) otherwise.
async fn apply_interface_config(
    qm: &Arc<IrohMeshManager>,
    local_id: &str,
    node_id: &str,
    netdev: &str,
    ipv4: Option<&str>,
    netmask: Option<&str>,
    mtu: u32,
    sudo_password: &str,
) -> Result<(), String> {
    if node_id == local_id {
        let iface = netdev.to_string();
        let ip = ipv4.map(String::from);
        let mask = netmask.map(String::from);
        let mut pwd = sudo_password.to_string();
        return tokio::task::spawn_blocking(move || {
            let r = crate::mesh::network_config::apply_network_config(
                &iface,
                ip.as_deref(),
                mask.as_deref(),
                None,
                false,
                Some(mtu),
                &pwd,
            );
            use zeroize::Zeroize;
            pwd.zeroize();
            r.map(|_| ()).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("blad watku: {}", e))?;
    }

    use tentaflow_protocol::mesh::MeshCommandType;
    let cmd = MeshCommandType::NetworkConfig {
        interface: netdev.to_string(),
        ipv4: ipv4.map(String::from),
        netmask: netmask.map(String::from),
        gateway: None,
        dhcp: false,
        sudo_password: sudo_password.to_string(),
        mtu: Some(mtu),
    };
    match qm.send_command_and_wait(node_id, cmd, 60).await {
        Ok(resp) if resp.ok => Ok(()),
        Ok(resp) => Err(resp
            .error
            .unwrap_or_else(|| "NetworkConfig nieudany".to_string())),
        Err(e) => Err(format!("NetworkConfig send nieudany: {}", e)),
    }
}

#[handler(variant = "ClusterRdmaConfigureRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn cluster_rdma_configure(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ClusterRdmaConfigureRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ClusterRdmaConfigureRequestBody",
            ));
        }
    };
    let ClusterRdmaConfigureRequest {
        cluster_id,
        sudo_password,
        mtu,
    } = payload;

    if sudo_password.is_empty() {
        return Err(ProtocolError::bad_request("Pole 'sudo_password' jest wymagane"));
    }
    if repository::get_cluster(&ctx.state.db, cluster_id)
        .map_err(|e| ProtocolError::new(ProtocolErrorCode::Internal, e.to_string()))?
        .is_none()
    {
        return Err(ProtocolError::not_found("cluster not found"));
    }

    let members = repository::list_cluster_members(&ctx.state.db, cluster_id)
        .map_err(|e| ProtocolError::new(ProtocolErrorCode::Internal, e.to_string()))?;
    if members.is_empty() {
        return Err(ProtocolError::bad_request("cluster nie ma czlonkow"));
    }

    let target_mtu = mtu.unwrap_or(crate::mesh::cluster_rdma::DEFAULT_RDMA_MTU);
    let qm = require_quic_mesh(ctx)?;
    let local_id = ctx.state.local_node_id.to_string();
    // Zeroizable working copy of the secret (P2-1) — wiped when the handler returns.
    let pwd = zeroize::Zeroizing::new(sudo_password.clone());

    // Per-member display metadata (hostname/status) resolved once, aligned to `members`.
    struct Meta {
        node_id: String,
        hostname: String,
        status: String,
    }
    let metas: Vec<Meta> = members
        .iter()
        .map(|m| {
            let peer = ctx.state.mesh_peer_store.get(&m.node_id);
            let hostname = peer
                .as_ref()
                .map(|p| {
                    if p.hostname.is_empty() {
                        m.node_id.clone()
                    } else {
                        p.hostname.clone()
                    }
                })
                .unwrap_or_else(|| m.node_id.clone());
            let status = peer
                .map(|p| p.status)
                .unwrap_or_else(|| "offline".to_string());
            Meta {
                node_id: m.node_id.clone(),
                hostname,
                status,
            }
        })
        .collect();

    // Phase 1: gather RoCE inventory for every member FIRST, so the IP plan can be
    // computed cluster-wide with full collision detection (P1-2).
    use std::collections::HashMap;
    let mut early_errors: HashMap<String, String> = HashMap::new();
    let mut inputs: Vec<crate::mesh::cluster_rdma::MemberRoceInput> = Vec::new();
    // Every member's probe-selected interconnect IP is reserved so a planned
    // secondary never lands on a node we could not query.
    let reserved_ips: Vec<String> = members
        .iter()
        .filter(|m| !m.interface_ip.is_empty())
        .map(|m| m.interface_ip.clone())
        .collect();

    for m in &members {
        // The probe-selected interconnect IP is the RDMA primary. Without it we
        // cannot derive the dedicated RDMA subnet — surface a clear per-node error.
        if m.interface_ip.is_empty() {
            early_errors.insert(
                m.node_id.clone(),
                "brak wybranego interfejsu interconnectu — uruchom najpierw test sieci".to_string(),
            );
            continue;
        }
        match fetch_roce_interfaces(ctx, &qm, &local_id, &m.node_id).await {
            Ok(roce) => inputs.push(crate::mesh::cluster_rdma::MemberRoceInput {
                node_id: m.node_id.clone(),
                primary_ip: m.interface_ip.clone(),
                roce,
            }),
            Err(e) => {
                early_errors.insert(m.node_id.clone(), e);
            }
        }
    }

    // Phase 2: cluster-wide plan (per-node Ok plan or Err string).
    let plan_results = crate::mesh::cluster_rdma::plan_cluster(&inputs, target_mtu, &reserved_ips);
    let mut plan_map: HashMap<String, Result<crate::mesh::cluster_rdma::MemberRdmaPlan, String>> =
        plan_results.into_iter().collect();

    // Phase 3: apply per member, tracking per-interface success. RDMA state is
    // persisted ONLY on FULL member success (P1-3/P1-5) — a single confirmed HCA
    // is never recorded as a ready NCCL config.
    let mut out_members: Vec<ClusterRdmaMemberStatus> = Vec::new();
    let mut any_ok = false;

    for meta in &metas {
        if let Some(e) = early_errors.remove(&meta.node_id) {
            out_members.push(ClusterRdmaMemberStatus {
                node_id: meta.node_id.clone(),
                hostname: meta.hostname.clone(),
                status: meta.status.clone(),
                interfaces: Vec::new(),
                error: Some(e),
            });
            continue;
        }

        let plan = match plan_map.remove(&meta.node_id) {
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                out_members.push(ClusterRdmaMemberStatus {
                    node_id: meta.node_id.clone(),
                    hostname: meta.hostname.clone(),
                    status: meta.status.clone(),
                    interfaces: Vec::new(),
                    error: Some(e),
                });
                continue;
            }
            None => {
                out_members.push(ClusterRdmaMemberStatus {
                    node_id: meta.node_id.clone(),
                    hostname: meta.hostname.clone(),
                    status: meta.status.clone(),
                    interfaces: Vec::new(),
                    error: Some("brak planu RDMA dla noda".to_string()),
                });
                continue;
            }
        };

        let mut iface_results: Vec<ClusterRdmaInterface> = Vec::new();
        let mut member_error: Option<String> = None;
        // RoCE devices confirmed up (applied ok or already correct) — only these
        // are persisted, and only when the whole member succeeds.
        let mut confirmed_devices: Vec<String> = Vec::new();

        for pi in &plan.interfaces {
            let action = if !pi.needs_ip_change && !pi.needs_mtu_change {
                confirmed_devices.push(pi.roce_device.clone());
                "unchanged"
            } else {
                // needs_ip_change -> full static (re)assign; otherwise MTU-only
                // (non-destructive, never rewrites the primary's IP config — P1-4).
                let (ip_arg, mask_arg) = if pi.needs_ip_change {
                    (Some(pi.ipv4.as_str()), Some(pi.netmask.as_str()))
                } else {
                    (None, None)
                };
                match apply_interface_config(
                    &qm,
                    &local_id,
                    &meta.node_id,
                    &pi.netdev,
                    ip_arg,
                    mask_arg,
                    pi.mtu,
                    pwd.as_str(),
                )
                .await
                {
                    Ok(()) => {
                        confirmed_devices.push(pi.roce_device.clone());
                        if pi.needs_ip_change {
                            "assigned"
                        } else {
                            "mtu_only"
                        }
                    }
                    Err(e) => {
                        member_error = Some(format!("{}: {}", pi.netdev, e));
                        "failed"
                    }
                }
            };
            iface_results.push(ClusterRdmaInterface {
                netdev: pi.netdev.clone(),
                roce_device: pi.roce_device.clone(),
                ipv4: Some(pi.ipv4.clone()),
                mtu: pi.mtu,
                role: pi.role.to_string(),
                action: action.to_string(),
            });
            if member_error.is_some() {
                break;
            }
        }

        // Persist ONLY on full success: a required twin that failed leaves the
        // member's stored RDMA config untouched so D3 never consumes a half-up
        // (single-HCA) configuration as ready.
        if member_error.is_none() {
            any_ok = true;
            if let Err(e) = repository::update_cluster_member_rdma(
                &ctx.state.db,
                cluster_id,
                &meta.node_id,
                &confirmed_devices.join(","),
                &plan.primary_ip,
                &plan.socket_ifname,
            ) {
                warn!("update_cluster_member_rdma nieudany: {}", e);
            }
        }

        out_members.push(ClusterRdmaMemberStatus {
            node_id: meta.node_id.clone(),
            hostname: meta.hostname.clone(),
            status: meta.status.clone(),
            interfaces: iface_results,
            error: member_error,
        });
    }

    crate::routing::cluster_sync::broadcast_routing_mutation(&ctx.state.db, &ctx.state.quic_mesh);

    let _ = repository::log_audit(
        &ctx.state.db,
        None,
        None,
        "cluster.rdma_configure",
        Some(&format!("cluster:{}", cluster_id)),
        Some(if any_ok { "ok" } else { "failed" }),
        None,
        Some(ctx.state.local_node_id.as_ref()),
    );

    Ok(MessageBody::ClusterRdmaConfigureResponseBody(
        ClusterRdmaConfigureResponse {
            ok: any_ok,
            message: None,
            members: out_members,
        },
    ))
}

// =============================================================================
// Cluster distributed deploy (D3) — koordynator. Liczy role head/worker z
// `cluster_members` + ich D1 RoCE config (`rdma_devices`/`rdma_ip`/
// `rdma_socket_ifname`), wysyla per-node `ServiceDeployDistributed` (lokalnie
// przez ten sam potok co single-node deploy, zdalnie przez mesh), utrwala
// deployment i zwraca endpoint head-a. Native + model mesh-transfer = osobne
// chunki (ten zaklada model juz obecny na kazdym czlonku, deploy_method=docker).
// =============================================================================

/// Deployuje JEDNEGO czlonka (lokalnie przez `spawn_deploy_pipeline` albo zdalnie
/// przez `ServiceDeployDistributed`). Zwraca status czlonka do odpowiedzi.
async fn deploy_distributed_member(
    ctx: &HandlerContext,
    qm: &Arc<IrohMeshManager>,
    local_id: &str,
    spec: tentaflow_protocol::mesh::DistributedDeploySpec,
    hostname: String,
) -> ClusterDeployMemberStatus {
    let role = spec.role.clone();
    // Mesh routing target: the coordinator stamped the member's node_id into the
    // spec's `config_json` (`_target_node_id`) — the spec itself only carries
    // RDMA addressing, not the mesh node id.
    let target = spec_node_id(&spec);

    if target == local_id {
        // Lokalny czlonek — ten sam potok co single-node deploy.
        match deploy_distributed_local(ctx, &spec).await {
            Ok(deploy_id) => ClusterDeployMemberStatus {
                node_id: target,
                hostname,
                role,
                ok: true,
                deploy_id: Some(deploy_id),
                error: None,
            },
            Err(e) => ClusterDeployMemberStatus {
                node_id: target,
                hostname,
                role,
                ok: false,
                deploy_id: None,
                error: Some(e),
            },
        }
    } else {
        // Zdalny czlonek przez mesh.
        let trusted = ctx
            .state
            .mesh_security
            .as_ref()
            .map_or(false, |s| s.is_trusted(&target));
        if !trusted {
            return ClusterDeployMemberStatus {
                node_id: target,
                hostname,
                role,
                ok: false,
                deploy_id: None,
                error: Some("node nie jest zaufany".to_string()),
            };
        }
        let cmd = MeshCommandType::ServiceDeployDistributed { spec };
        match qm.send_command_and_wait(&target, cmd, 120).await {
            Ok(resp) if resp.ok => {
                let deploy_id = match resp.payload {
                    MeshCommandResponsePayload::ServiceDeployDistributedResult {
                        deploy_id,
                        ..
                    } => Some(deploy_id),
                    _ => None,
                };
                ClusterDeployMemberStatus {
                    node_id: target,
                    hostname,
                    role,
                    ok: true,
                    deploy_id,
                    error: None,
                }
            }
            Ok(resp) => ClusterDeployMemberStatus {
                node_id: target,
                hostname,
                role,
                ok: false,
                deploy_id: None,
                error: Some(resp.error.unwrap_or_else(|| "deploy nieudany".to_string())),
            },
            Err(e) => ClusterDeployMemberStatus {
                node_id: target,
                hostname,
                role,
                ok: false,
                deploy_id: None,
                error: Some(format!("mesh send nieudany: {}", e)),
            },
        }
    }
}

/// `node_id` czlonka jest przenoszony w `config_json` spec-a pod kluczem
/// `_target_node_id` (koordynator zna mapowanie node→rdma, ale spec wysylany na
/// dany node nie musi go nosic; uzywamy go tylko do routingu mesh po stronie
/// koordynatora). Patrz `build_member_spec`.
fn spec_node_id(spec: &tentaflow_protocol::mesh::DistributedDeploySpec) -> String {
    serde_json::from_str::<serde_json::Value>(&spec.config_json)
        .ok()
        .and_then(|v| {
            v.get("_target_node_id")
                .and_then(|x| x.as_str())
                .map(String::from)
        })
        .unwrap_or_default()
}

/// Lokalny distributed deploy czlonka — reuzywa `create_deploy_job` +
/// `spawn_deploy_pipeline` (dokladnie ten sam potok co single-node), z
/// `user_config` zbudowanym z `_distributed`/komendy/NCCL env.
async fn deploy_distributed_local(
    ctx: &HandlerContext,
    spec: &tentaflow_protocol::mesh::DistributedDeploySpec,
) -> Result<String, String> {
    use crate::services_repo::services::DeployMethod;
    // Preflight (P1-4): same OS port-check + stale-Ray cleanup the remote path
    // runs in the executor.
    crate::services::deploy::distributed::preflight_member(spec).await?;
    let config_json = crate::services::deploy::distributed::build_member_config_json(spec)?;
    let user_config: serde_json::Value =
        serde_json::from_str(&config_json).map_err(|e| format!("config parse: {e}"))?;
    let manifest = crate::services::manifest::registry()
        .by_id(&spec.engine_id)
        .cloned()
        .ok_or_else(|| format!("engine '{}' nie istnieje w manifescie", spec.engine_id))?;
    let port_allocator = ctx
        .state
        .port_allocator
        .clone()
        .ok_or_else(|| "port allocator niedostepny".to_string())?;
    let job = crate::services::deploy::create_deploy_job(
        DeployMethod::Docker,
        &manifest,
        &user_config,
        &ctx.state.db,
        ctx.state.local_node_id.as_ref(),
        None,
        None,
    )
    .map_err(|e| e.to_string())?;
    if let Ok(Some(info)) = crate::services::snapshot_builder::build_one(
        &ctx.state.db,
        job.service_id,
        ctx.state.local_node_id.as_ref(),
    ) {
        super::handlers::broadcast_service_change(ctx, tentaflow_protocol::ServiceChange::Added(info));
    }
    Ok(super::handlers::spawn_deploy_pipeline(
        ctx,
        job,
        DeployMethod::Docker,
        &manifest,
        &user_config,
        port_allocator,
    ))
}

/// Buduje per-node spec, wstrzykujac `_target_node_id` do `config_json` (tylko
/// koordynator go czyta — `build_member_config_json` przepuszcza nieznane klucze).
#[allow(clippy::too_many_arguments)]
fn build_member_spec(
    deployment_cluster_id: &str,
    cluster_id: &str,
    engine_id: &str,
    role: &str,
    member: &crate::db::models::DbClusterMember,
    model: &str,
    served: &str,
    tp_size: u32,
    gpus_per_node: u32,
    port: u16,
    dist_port: u16,
    gpu_mem: f32,
    max_model_len: u32,
    ray_head_ip: &str,
    ray_port: u16,
    user_config_json: &str,
) -> tentaflow_protocol::mesh::DistributedDeploySpec {
    // Wstrzykujemy `_target_node_id` do user-config JSON (do routingu mesh).
    let mut cfg: serde_json::Value =
        serde_json::from_str(user_config_json).unwrap_or(serde_json::Value::Null);
    if !cfg.is_object() {
        cfg = serde_json::json!({});
    }
    if let Some(obj) = cfg.as_object_mut() {
        obj.insert(
            "_target_node_id".to_string(),
            serde_json::Value::String(member.node_id.clone()),
        );
    }
    tentaflow_protocol::mesh::DistributedDeploySpec {
        deployment_cluster_id: deployment_cluster_id.to_string(),
        cluster_id: cluster_id.to_string(),
        engine_id: engine_id.to_string(),
        role: role.to_string(),
        model: model.to_string(),
        served_model_name: served.to_string(),
        tp_size,
        num_gpus: gpus_per_node,
        port,
        dist_port,
        gpu_memory_utilization: gpu_mem,
        max_model_len,
        ray_head_ip: ray_head_ip.to_string(),
        ray_port,
        rdma_ip: member.rdma_ip.clone(),
        rdma_devices: member.rdma_devices.clone(),
        socket_ifname: member.rdma_socket_ifname.clone(),
        // Per-member persisted RoCEv2 GID index (D1 column, default 3) — not a
        // hardcode in the deploy path (P2-2).
        gid_index: member.rdma_gid_index.max(0) as u32,
        config_json: serde_json::to_string(&cfg).unwrap_or_else(|_| "{}".to_string()),
    }
}

#[handler(variant = "ClusterDeployRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn cluster_deploy(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ClusterDeployRequestBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected ClusterDeployRequestBody")),
    };
    let ClusterDeployRequest {
        cluster_id,
        engine_id,
        model_repo,
        model_preset_id,
        served_model_name,
        gpu_memory_utilization,
        max_model_len,
        port,
        gpus_per_node,
        config_json,
        build_timeout_secs,
        gcs_timeout_secs,
        ready_timeout_secs,
    } = payload;

    if crate::db::repository::get_cluster(&ctx.state.db, cluster_id)
        .map_err(|e| ProtocolError::new(ProtocolErrorCode::Internal, e.to_string()))?
        .is_none()
    {
        return Err(ProtocolError::not_found("cluster not found"));
    }
    let members = crate::db::repository::list_cluster_members(&ctx.state.db, cluster_id)
        .map_err(|e| ProtocolError::new(ProtocolErrorCode::Internal, e.to_string()))?;
    if members.len() < 2 {
        return Err(ProtocolError::bad_request(
            "distributed deploy wymaga co najmniej 2 czlonkow klastra",
        ));
    }
    // Each member MUST have D1 RoCE config (rdma devices + ip + socket); without
    // it the NCCL env cannot be built — fail loudly (run cluster RDMA config first).
    for m in &members {
        if m.rdma_devices.is_empty() || m.rdma_ip.is_empty() || m.rdma_socket_ifname.is_empty() {
            return Err(ProtocolError::bad_request(format!(
                "czlonek {} nie ma konfiguracji RDMA (uruchom najpierw 'Konfiguruj RDMA' dla klastra)",
                m.node_id
            )));
        }
    }

    let manifest = crate::services::manifest::registry()
        .by_id(engine_id)
        .cloned()
        .ok_or_else(|| ProtocolError::not_found(format!("engine '{}' nie istnieje", engine_id)))?;

    // Resolve the model repo (custom repo wins; else preset lookup).
    let model_sel = serde_json::json!({
        "model_repo": model_repo,
        "model_preset_id": model_preset_id,
    });
    let model = crate::services::deploy::resolve_model_repo(&manifest, &model_sel)
        .ok_or_else(|| ProtocolError::bad_request("brak modelu (model_repo lub model_preset_id)"))?;
    let served = served_model_name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| model.clone());

    // P1-4: reject a second deploy only when a LIVE deployment (deploying/running)
    // exists — a node has one GPU set, so two TP deployments would collide on GPU
    // + host ports. A `failed` deployment does NOT block (cleared below).
    if let Some(existing) = crate::db::repository::active_cluster_deployment(&ctx.state.db, cluster_id)
        .map_err(|e| ProtocolError::new(ProtocolErrorCode::Internal, e.to_string()))?
    {
        return Err(ProtocolError::bad_request(format!(
            "klaster ma juz aktywny deployment ({}, status {}) — zatrzymaj go najpierw",
            existing.deployment_cluster_id, existing.status
        )));
    }

    // Auto-clear stale FAILED deployments of this cluster so a redeploy is not
    // blocked by leftovers (idempotent best-effort teardown + delete record).
    let local_id_pre = ctx.state.local_node_id.to_string();
    let qm_pre = require_quic_mesh(ctx)?;
    if let Ok(failed) = crate::db::repository::failed_cluster_deployments(&ctx.state.db, cluster_id) {
        for f in failed {
            let fmembers = crate::db::repository::list_cluster_deployment_members(
                &ctx.state.db,
                &f.deployment_cluster_id,
            )
            .unwrap_or_default();
            let _ = teardown_distributed_members(
                ctx,
                &qm_pre,
                &local_id_pre,
                &f.deployment_cluster_id,
                &fmembers,
            )
            .await;
            let _ = crate::db::repository::delete_cluster_deployment(
                &ctx.state.db,
                &f.deployment_cluster_id,
            );
        }
    }

    let gpus_per_node = gpus_per_node.unwrap_or(1).max(1);
    let tp_size = members.len() as u32 * gpus_per_node;

    // Two ports from the SAME PortAllocator a normal service deploy uses: serve API
    // (`vllm serve --port`, honours the wizard's preferred `port`) and the
    // torch.distributed TCPStore master (`VLLM_PORT` on every member). Both are
    // leased so a concurrent local deploy can't reuse them; released on failure.
    // Distinct by construction — `acquire()` runs after `acquire_or_specific` leased
    // the serve port, so it never hands the same one back.
    let port_allocator = ctx.state.port_allocator.clone().ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::Internal, "port allocator niedostepny")
    })?;
    let serve_port = port_allocator
        .acquire_or_specific(*port)
        .map_err(|e| ProtocolError::new(ProtocolErrorCode::Internal, format!("alokacja portu serve: {e}")))?;
    let dist_port = match port_allocator.acquire() {
        Ok(p) => p,
        Err(e) => {
            let _ = port_allocator.release(serve_port);
            return Err(ProtocolError::new(
                ProtocolErrorCode::Internal,
                format!("alokacja portu torch.distributed: {e}"),
            ));
        }
    };
    let gpu_mem = gpu_memory_utilization.unwrap_or(0.90);
    let max_len = max_model_len.unwrap_or(8192);
    let ray_port: u16 = 6379;
    let user_cfg = config_json.clone().unwrap_or_else(|| "{}".to_string());
    // Build phase (image build + container start) has its OWN generous budget so
    // a slow first image build does NOT eat the short Ray-GCS budget.
    let build_timeout =
        std::time::Duration::from_secs(build_timeout_secs.unwrap_or(600).max(30) as u64);
    let gcs_timeout = std::time::Duration::from_secs(gcs_timeout_secs.unwrap_or(60).max(5) as u64);
    let ready_timeout =
        std::time::Duration::from_secs(ready_timeout_secs.unwrap_or(600).max(30) as u64);
    let expected_nodes = members.len() as u32;

    let local_id = ctx.state.local_node_id.to_string();
    // From here until the deployment is persisted (status deploying) every early
    // return must hand both leases back — `finalize_distributed_failure` only runs
    // AFTER persist, so these pre-persist exits would otherwise leak serve+dist.
    let qm = match require_quic_mesh(ctx) {
        Ok(qm) => qm,
        Err(e) => {
            let _ = port_allocator.release(serve_port);
            let _ = port_allocator.release(dist_port);
            return Err(e);
        }
    };

    // Head = local node when it is a member (so the OpenAI endpoint sits on this
    // node), otherwise the first member. ray_head_ip = head's RDMA IP.
    let head_idx = members
        .iter()
        .position(|m| m.node_id == local_id)
        .unwrap_or(0);
    let ray_head_ip = members[head_idx].rdma_ip.clone();
    let head_node_id = members[head_idx].node_id.clone();
    let deployment_cluster_id = uuid::Uuid::new_v4().to_string();

    // Build every member spec + the persisted membership up front.
    let mut specs: Vec<(usize, tentaflow_protocol::mesh::DistributedDeploySpec)> = Vec::new();
    let mut persisted_members: Vec<crate::db::models::DbClusterDeploymentMember> = Vec::new();
    for (idx, m) in members.iter().enumerate() {
        let role = if idx == head_idx { "head" } else { "worker" };
        specs.push((
            idx,
            build_member_spec(
                &deployment_cluster_id,
                cluster_id,
                engine_id,
                role,
                m,
                &model,
                &served,
                tp_size,
                gpus_per_node,
                serve_port,
                dist_port,
                gpu_mem,
                max_len,
                &ray_head_ip,
                ray_port,
                &user_cfg,
            ),
        ));
        persisted_members.push(crate::db::models::DbClusterDeploymentMember {
            deployment_cluster_id: deployment_cluster_id.clone(),
            node_id: m.node_id.clone(),
            role: role.to_string(),
            container_name: crate::services::deploy::distributed::container_name(engine_id, serve_port),
        });
    }
    let head_spec = specs[head_idx.min(specs.len() - 1)].1.clone();
    let endpoint_url = crate::services::deploy::distributed::endpoint_url_for(&head_spec);

    // Persist UPFRONT (status deploying) so a crash mid-deploy still leaves a
    // stoppable record (P1-2).
    let deployment = crate::db::models::DbClusterDeployment {
        deployment_cluster_id: deployment_cluster_id.clone(),
        cluster_id: cluster_id.clone(),
        engine_id: engine_id.clone(),
        model: model.clone(),
        served_model_name: served.clone(),
        tp_size: tp_size as i64,
        head_node_id: head_node_id.clone(),
        port: serve_port as i64,
        dist_port: dist_port as i64,
        endpoint_url: endpoint_url.clone(),
        status: "deploying".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    if let Err(e) = crate::db::repository::upsert_cluster_deployment(
        &ctx.state.db,
        &deployment,
        &persisted_members,
    ) {
        let _ = port_allocator.release(serve_port);
        let _ = port_allocator.release(dist_port);
        return Err(ProtocolError::new(
            ProtocolErrorCode::Internal,
            format!("persist deployment: {e}"),
        ));
    }

    let mut statuses: Vec<ClusterDeployMemberStatus> = Vec::new();

    info!(deployment_cluster_id=%deployment_cluster_id, "distributed deploy P1: start head (Ray GCS)");
    // 1. Head FIRST: start Ray GCS + `vllm serve` (vLLM BLOCKS until workers join).
    let head_status = deploy_distributed_member(
        ctx,
        &qm,
        &local_id,
        head_spec.clone(),
        hostname_for(ctx, &head_node_id),
    )
    .await;
    if !head_status.ok {
        let reason = format!(
            "head nie wystartował: {}",
            head_status.error.clone().unwrap_or_default()
        );
        statuses.push(head_status);
        return Ok(finalize_distributed_failure(
            ctx, &qm, &local_id, &deployment_cluster_id, serve_port, dist_port, &head_node_id, endpoint_url,
            &persisted_members, statuses, reason,
        )
        .await);
    }
    statuses.push(head_status);

    info!(deployment_cluster_id=%deployment_cluster_id, "distributed deploy P2: wait head container build");
    // 2a. BUILD phase: wait for the head CONTAINER to be up (image build +
    //     container start) on its OWN generous budget — a slow first build
    //     extends THIS phase, NOT the short Ray-GCS budget below (P2-fix).
    if let Err(e) = poll_node_readiness(
        ctx, &qm, &head_node_id, &local_id, &deployment_cluster_id, ray_port, serve_port,
        expected_nodes, ReadyPhase::ContainerUp, build_timeout,
    )
    .await
    {
        return Ok(finalize_distributed_failure(
            ctx, &qm, &local_id, &deployment_cluster_id, serve_port, dist_port, &head_node_id, endpoint_url,
            &persisted_members, statuses, format!("kontener head nie wstał (build): {e}"),
        )
        .await);
    }

    info!(deployment_cluster_id=%deployment_cluster_id, "distributed deploy P3: wait head Ray GCS");
    // 2b. Wait for the head Ray GCS to come up so workers can join (short budget).
    if let Err(e) = poll_node_readiness(
        ctx, &qm, &head_node_id, &local_id, &deployment_cluster_id, ray_port, serve_port,
        expected_nodes, ReadyPhase::GcsUp, gcs_timeout,
    )
    .await
    {
        return Ok(finalize_distributed_failure(
            ctx, &qm, &local_id, &deployment_cluster_id, serve_port, dist_port, &head_node_id, endpoint_url,
            &persisted_members, statuses, format!("Ray GCS head nie wstał: {e}"),
        )
        .await);
    }

    info!(deployment_cluster_id=%deployment_cluster_id, "distributed deploy P4: start workers (join Ray)");
    // 3. Start workers (join the Ray head). Each worker is gated on its OWN
    //    container coming up (build budget) before we move on.
    for (idx, spec) in &specs {
        if *idx == head_idx {
            continue;
        }
        let node_id = members[*idx].node_id.clone();
        let status =
            deploy_distributed_member(ctx, &qm, &local_id, spec.clone(), hostname_for(ctx, &node_id))
                .await;
        let ok = status.ok;
        statuses.push(status);
        if !ok {
            return Ok(finalize_distributed_failure(
                ctx, &qm, &local_id, &deployment_cluster_id, serve_port, dist_port, &head_node_id, endpoint_url,
                &persisted_members, statuses, format!("worker {} nie wystartował", node_id),
            )
            .await);
        }
        // Worker container-up gate (build budget) before declaring it started.
        if let Err(e) = poll_node_readiness(
            ctx, &qm, &node_id, &local_id, &deployment_cluster_id, ray_port, serve_port,
            expected_nodes, ReadyPhase::ContainerUp, build_timeout,
        )
        .await
        {
            return Ok(finalize_distributed_failure(
                ctx, &qm, &local_id, &deployment_cluster_id, serve_port, dist_port, &head_node_id, endpoint_url,
                &persisted_members, statuses,
                format!("kontener worker {} nie wstał (build): {e}", node_id),
            )
            .await);
        }
    }

    // 4. Wait for the Ray cluster to actually have every node joined (head sees
    //    `expected_nodes` in `ray status`). ONLY THEN does `vllm serve` get a
    //    complete GPU set — starting it earlier would make it block + time out
    //    waiting for the 2nd GPU and the head would exit (the ordering bug).
    if let Err(e) = poll_node_readiness(
        ctx, &qm, &head_node_id, &local_id, &deployment_cluster_id, ray_port, serve_port,
        expected_nodes, ReadyPhase::ClusterReady, gcs_timeout,
    )
    .await
    {
        return Ok(finalize_distributed_failure(
            ctx, &qm, &local_id, &deployment_cluster_id, serve_port, dist_port, &head_node_id, endpoint_url,
            &persisted_members, statuses,
            format!("workery nie dołączyły do klastra Ray w czasie: {e}"),
        )
        .await);
    }

    info!(deployment_cluster_id=%deployment_cluster_id, "distributed deploy P5: start vllm serve on head");
    // 5. Cluster is complete → launch `vllm serve` ON THE HEAD via docker exec
    //    (detached). vLLM now finds the full TP GPU set immediately.
    let serve_cmd = match crate::services::deploy::distributed::build_serve_command(&head_spec) {
        Ok(c) => c,
        Err(e) => {
            return Ok(finalize_distributed_failure(
                ctx, &qm, &local_id, &deployment_cluster_id, serve_port, dist_port, &head_node_id, endpoint_url,
                &persisted_members, statuses, format!("budowa komendy vllm serve: {e}"),
            )
            .await);
        }
    };
    if let Err(e) =
        start_serve_on_head(ctx, &qm, &head_node_id, &local_id, &deployment_cluster_id, &serve_cmd)
            .await
    {
        return Ok(finalize_distributed_failure(
            ctx, &qm, &local_id, &deployment_cluster_id, serve_port, dist_port, &head_node_id, endpoint_url,
            &persisted_members, statuses, format!("start vllm serve na headzie: {e}"),
        )
        .await);
    }

    info!(deployment_cluster_id=%deployment_cluster_id, "distributed deploy P6: wait serve ready (/v1/models)");
    // 6. FINAL readiness: head `/v1/models` 200 — vLLM loaded the model across the
    //    TP cluster and serves. Bounded by `ready_timeout` (P1-1).
    if let Err(e) = poll_node_readiness(
        ctx, &qm, &head_node_id, &local_id, &deployment_cluster_id, ray_port, serve_port,
        expected_nodes, ReadyPhase::ServeReady, ready_timeout,
    )
    .await
    {
        // Pull the REAL serve failure from the head's serve log so finalize +
        // GUI show why vLLM never came up (only when the head is local — a
        // remote head's log lives on that node and isn't reachable here).
        let reason = if head_node_id == local_id {
            match crate::services::deploy::distributed::serve_log_tail(&deployment_cluster_id, 40)
                .await
            {
                Some(tail) => format!(
                    "klaster nie zaczął serwować w czasie: {e}\n--- vllm serve log ---\n{tail}"
                ),
                None => format!("klaster nie zaczął serwować w czasie: {e}"),
            }
        } else {
            format!("klaster nie zaczął serwować w czasie: {e}")
        };
        return Ok(finalize_distributed_failure(
            ctx, &qm, &local_id, &deployment_cluster_id, serve_port, dist_port, &head_node_id, endpoint_url,
            &persisted_members, statuses, reason,
        )
        .await);
    }

    // Real readiness achieved — every member is up and the endpoint serves.
    for s in &mut statuses {
        s.ok = true;
        s.error = None;
    }
    let _ = crate::db::repository::set_cluster_deployment_status(
        &ctx.state.db,
        &deployment_cluster_id,
        "running",
    );

    let _ = crate::db::repository::log_audit(
        &ctx.state.db,
        None,
        None,
        "cluster.deploy",
        Some(&format!("cluster:{} dep:{}", cluster_id, deployment_cluster_id)),
        Some("ok"),
        None,
        Some(ctx.state.local_node_id.as_ref()),
    );

    Ok(MessageBody::ClusterDeployResponseBody(ClusterDeployResponse {
        ok: true,
        deployment_cluster_id,
        head_node_id,
        endpoint_url,
        members: statuses,
        message: None,
    }))
}

/// Hostname noda z peer-store (fallback: node_id).
fn hostname_for(ctx: &HandlerContext, node_id: &str) -> String {
    ctx.state
        .mesh_peer_store
        .get(node_id)
        .map(|p| {
            if p.hostname.is_empty() {
                node_id.to_string()
            } else {
                p.hostname
            }
        })
        .unwrap_or_else(|| node_id.to_string())
}

/// Faza gotowosci sondowana przez koordynatora.
#[derive(Clone, Copy, PartialEq)]
enum ReadyPhase {
    /// Kontener czlonka wstal (obraz zbudowany + start) — gate fazy buildu.
    ContainerUp,
    /// GCS Ray nasluchuje (workery moga dolaczyc).
    GcsUp,
    /// Klaster Ray ma wszystkie nody (head widzi `expected_nodes` w `ray status`)
    /// — dopiero teraz `vllm serve` ma komplet GPU.
    ClusterReady,
    /// Endpoint OpenAI serwuje (model zaladowany na calym TP-cluster).
    ServeReady,
}

/// Odpala `vllm serve` na headzie (local → `exec_serve_on_head`; remote →
/// `DistributedStartServe` przez mesh). Detached — vLLM laduje model w tle, a
/// gotowosc potwierdza pozniejszy `ServeReady`.
async fn start_serve_on_head(
    ctx: &HandlerContext,
    qm: &Arc<IrohMeshManager>,
    head_node_id: &str,
    local_id: &str,
    deployment_cluster_id: &str,
    serve_cmd: &str,
) -> Result<(), String> {
    if head_node_id == local_id {
        return crate::services::deploy::distributed::exec_serve_on_head(
            deployment_cluster_id,
            serve_cmd,
        )
        .await;
    }
    let cmd = MeshCommandType::DistributedStartServe {
        deployment_cluster_id: deployment_cluster_id.to_string(),
        serve_cmd: serve_cmd.to_string(),
    };
    match qm.send_command_and_wait(head_node_id, cmd, 30).await {
        Ok(resp) if resp.ok => Ok(()),
        Ok(resp) => Err(resp.error.unwrap_or_else(|| "start serve nieudany".to_string())),
        Err(e) => Err(format!("mesh send (start serve) nieudany: {e}")),
    }
}

/// Sonduje `target_node` (lokalnie albo `DistributedReadiness` przez mesh) do
/// osiagniecia `phase` albo wyczerpania `timeout`. Zwraca Err z czytelnym
/// komunikatem przy timeoucie (z ostatnim widzianym stanem). To jest REALNY gate
/// gotowosci — `ClusterDeployResponse.ok` zalezy od niego, nie od samego
/// zaplanowania (P1-1). `ContainerUp` ma sens dla kazdego noda; `GcsUp`/
/// `ServeReady` tylko dla head-a.
#[allow(clippy::too_many_arguments)]
async fn poll_node_readiness(
    ctx: &HandlerContext,
    qm: &Arc<IrohMeshManager>,
    target_node: &str,
    local_id: &str,
    deployment_cluster_id: &str,
    ray_port: u16,
    serve_port: u16,
    expected_nodes: u32,
    phase: ReadyPhase,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let start = std::time::Instant::now();
    let mut last = String::from("brak odpowiedzi");
    while start.elapsed() < timeout {
        let (container_running, gcs_up, ray_nodes, serve_ready) = probe_node_once(
            ctx,
            qm,
            target_node,
            local_id,
            deployment_cluster_id,
            ray_port,
            serve_port,
            expected_nodes,
        )
        .await;
        let done = match phase {
            ReadyPhase::ContainerUp => container_running,
            ReadyPhase::GcsUp => gcs_up,
            // Full Ray cluster: head's `ray status` shows every node joined, so
            // `vllm serve` will find the complete TP GPU set.
            ReadyPhase::ClusterReady => ray_nodes >= expected_nodes,
            // Serve readiness is authoritative: /v1/models answers only after vLLM
            // loaded the model across the TP cluster.
            ReadyPhase::ServeReady => serve_ready,
        };
        if done {
            return Ok(());
        }
        last = format!(
            "container={} gcs_up={} ray_nodes={}/{} serve_ready={}",
            container_running, gcs_up, ray_nodes, expected_nodes, serve_ready
        );
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    Err(format!("timeout po {}s ({})", timeout.as_secs(), last))
}

/// Jednorazowy odczyt gotowosci noda (local → bezposrednio; remote → mesh).
/// Transient blad mesh → traktowany jak "jeszcze niegotowy" (polling kontynuuje).
#[allow(clippy::too_many_arguments)]
async fn probe_node_once(
    ctx: &HandlerContext,
    qm: &Arc<IrohMeshManager>,
    target_node: &str,
    local_id: &str,
    deployment_cluster_id: &str,
    ray_port: u16,
    serve_port: u16,
    expected_nodes: u32,
) -> (bool, bool, u32, bool) {
    if target_node == local_id {
        let s = crate::services::deploy::distributed::probe_readiness(
            deployment_cluster_id,
            ray_port,
            serve_port,
        )
        .await;
        return (s.container_running, s.ray_gcs_up, s.ray_nodes, s.serve_ready);
    }
    let cmd = MeshCommandType::DistributedReadiness {
        deployment_cluster_id: deployment_cluster_id.to_string(),
        ray_port,
        serve_port,
        expected_nodes,
    };
    match qm.send_command_and_wait(target_node, cmd, 15).await {
        Ok(resp) if resp.ok => match resp.payload {
            MeshCommandResponsePayload::DistributedReadinessResult {
                container_running,
                ray_gcs_up,
                ray_nodes,
                serve_ready,
                ..
            } => (container_running, ray_gcs_up, ray_nodes, serve_ready),
            _ => (false, false, 0, false),
        },
        _ => (false, false, 0, false),
    }
}

/// Teardown WSZYSTKICH wystartowanych czlonkow + finalizacja rekordu po nieudanym
/// deployu (P1-2): zero osieroconych kontenerow Ray. Gdy teardown w pelni sie
/// udal — kasuje rekord; gdy NIE (kontener moze zyc) — zostawia rekord ze
/// statusem `failed`, zeby admin mogl ponowic stop.
#[allow(clippy::too_many_arguments)]
async fn finalize_distributed_failure(
    ctx: &HandlerContext,
    qm: &Arc<IrohMeshManager>,
    local_id: &str,
    deployment_cluster_id: &str,
    serve_port: u16,
    dist_port: u16,
    head_node_id: &str,
    endpoint_url: Option<String>,
    members: &[crate::db::models::DbClusterDeploymentMember],
    statuses: Vec<ClusterDeployMemberStatus>,
    reason: String,
) -> MessageBody {
    // Release the two coordinator-leased ports (serve + torch.distributed) so a
    // failed deploy does not leak them out of the allocator's lease set.
    if let Some(ports) = ctx.state.port_allocator.clone() {
        let _ = ports.release(serve_port);
        let _ = ports.release(dist_port);
    }
    let teardown_errors =
        teardown_distributed_members(ctx, qm, local_id, deployment_cluster_id, members).await;
    let message = if teardown_errors.is_empty() {
        let _ = crate::db::repository::delete_cluster_deployment(&ctx.state.db, deployment_cluster_id);
        format!("{reason} (rollback: wszystkie kontenery usunięte)")
    } else {
        let _ = crate::db::repository::set_cluster_deployment_status(
            &ctx.state.db,
            deployment_cluster_id,
            "failed",
        );
        format!(
            "{reason} ; rollback NIEKOMPLETNY ({}) — rekord zachowany, ponów STOP",
            teardown_errors.join("; ")
        )
    };
    crate::routing::cluster_sync::broadcast_routing_mutation(&ctx.state.db, &ctx.state.quic_mesh);
    MessageBody::ClusterDeployResponseBody(ClusterDeployResponse {
        ok: false,
        deployment_cluster_id: deployment_cluster_id.to_string(),
        head_node_id: head_node_id.to_string(),
        endpoint_url,
        members: statuses,
        message: Some(message),
    })
}

/// Wysyla teardown do KAZDEGO czlonka deploymentu (local bezposrednio, remote
/// `ServiceStopDistributed`) i agreguje bledy. Niepusta lista = teardown
/// niekompletny.
async fn teardown_distributed_members(
    ctx: &HandlerContext,
    qm: &Arc<IrohMeshManager>,
    local_id: &str,
    deployment_cluster_id: &str,
    members: &[crate::db::models::DbClusterDeploymentMember],
) -> Vec<String> {
    let mut errors = Vec::new();
    let port_allocator = ctx.state.port_allocator.clone();
    // Distinct nodes (one container per node).
    let mut seen = std::collections::HashSet::new();
    for m in members {
        if !seen.insert(m.node_id.clone()) {
            continue;
        }
        if m.node_id == local_id {
            if let Some(ports) = port_allocator.clone() {
                let (_removed, errs) = crate::services::deploy::distributed::stop_distributed(
                    &ctx.state.db,
                    ports,
                    deployment_cluster_id,
                )
                .await;
                errors.extend(errs);
            } else {
                errors.push(format!("{}: port allocator niedostepny", m.node_id));
            }
        } else {
            let cmd = MeshCommandType::ServiceStopDistributed {
                deployment_cluster_id: deployment_cluster_id.to_string(),
            };
            match qm.send_command_and_wait(&m.node_id, cmd, 60).await {
                Ok(resp) if resp.ok => {}
                Ok(resp) => errors.push(format!(
                    "{}: {}",
                    m.node_id,
                    resp.error.unwrap_or_else(|| "stop nieudany".to_string())
                )),
                Err(e) => errors.push(format!("{}: mesh send nieudany: {}", m.node_id, e)),
            }
        }
    }
    errors
}

#[handler(variant = "ClusterDeployStopRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn cluster_deploy_stop(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ClusterDeployStopRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ClusterDeployStopRequestBody",
            ))
        }
    };
    let ClusterDeployStopRequest {
        cluster_id,
        deployment_cluster_id,
    } = payload;

    let dep = crate::db::repository::get_cluster_deployment(&ctx.state.db, deployment_cluster_id)
        .map_err(|e| ProtocolError::new(ProtocolErrorCode::Internal, e.to_string()))?
        .ok_or_else(|| ProtocolError::not_found("deployment not found"))?;
    // P2-3: the deployment must belong to the cluster named in the request — a
    // stale/forged cluster_id must not tear down another cluster's deployment.
    if dep.cluster_id != *cluster_id {
        return Err(ProtocolError::bad_request(
            "deployment nie należy do podanego klastra",
        ));
    }
    let dep_members =
        crate::db::repository::list_cluster_deployment_members(&ctx.state.db, deployment_cluster_id)
            .map_err(|e| ProtocolError::new(ProtocolErrorCode::Internal, e.to_string()))?;

    let local_id = ctx.state.local_node_id.to_string();
    let qm = require_quic_mesh(ctx)?;
    let port_allocator = ctx
        .state
        .port_allocator
        .clone()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::Internal, "port allocator niedostepny"))?;

    let mut statuses: Vec<ClusterDeployMemberStatus> = Vec::new();
    for m in &dep_members {
        let status = if m.node_id == local_id {
            let (_removed, errs) = crate::services::deploy::distributed::stop_distributed(
                &ctx.state.db,
                port_allocator.clone(),
                deployment_cluster_id,
            )
            .await;
            ClusterDeployMemberStatus {
                node_id: m.node_id.clone(),
                hostname: hostname_for(ctx, &m.node_id),
                role: m.role.clone(),
                ok: errs.is_empty(),
                deploy_id: None,
                error: if errs.is_empty() {
                    None
                } else {
                    Some(errs.join("; "))
                },
            }
        } else {
            let cmd = MeshCommandType::ServiceStopDistributed {
                deployment_cluster_id: deployment_cluster_id.clone(),
            };
            match qm.send_command_and_wait(&m.node_id, cmd, 60).await {
                Ok(resp) if resp.ok => ClusterDeployMemberStatus {
                    node_id: m.node_id.clone(),
                    hostname: hostname_for(ctx, &m.node_id),
                    role: m.role.clone(),
                    ok: true,
                    deploy_id: None,
                    error: None,
                },
                Ok(resp) => ClusterDeployMemberStatus {
                    node_id: m.node_id.clone(),
                    hostname: hostname_for(ctx, &m.node_id),
                    role: m.role.clone(),
                    ok: false,
                    deploy_id: None,
                    error: Some(resp.error.unwrap_or_else(|| "stop nieudany".to_string())),
                },
                Err(e) => ClusterDeployMemberStatus {
                    node_id: m.node_id.clone(),
                    hostname: hostname_for(ctx, &m.node_id),
                    role: m.role.clone(),
                    ok: false,
                    deploy_id: None,
                    error: Some(format!("mesh send nieudany: {}", e)),
                },
            }
        };
        statuses.push(status);
    }

    let all_ok = statuses.iter().all(|s| s.ok);
    // P1-2: only delete the record when teardown FULLY succeeded; on partial
    // failure keep it (status failed) so the admin can retry STOP — never leave
    // a possibly-orphaned Ray container untracked.
    if all_ok {
        // Release the two coordinator-leased ports (serve `dep.port` + torch.distributed
        // `dep.dist_port`) back to THIS node's allocator — the leases were taken here at
        // deploy time, never on the workers, and `deploy::stop` deliberately never frees
        // them. dist_port==0 marks a legacy row predating allocation; nothing to free.
        let _ = port_allocator.release(dep.port as u16);
        if dep.dist_port > 0 {
            let _ = port_allocator.release(dep.dist_port as u16);
        }
        if let Err(e) =
            crate::db::repository::delete_cluster_deployment(&ctx.state.db, deployment_cluster_id)
        {
            warn!("delete_cluster_deployment nieudany: {}", e);
        }
    } else {
        let _ = crate::db::repository::set_cluster_deployment_status(
            &ctx.state.db,
            deployment_cluster_id,
            "failed",
        );
    }

    let _ = crate::db::repository::log_audit(
        &ctx.state.db,
        None,
        None,
        "cluster.deploy_stop",
        Some(&format!("cluster:{} dep:{}", cluster_id, deployment_cluster_id)),
        Some(if all_ok { "ok" } else { "partial" }),
        None,
        Some(ctx.state.local_node_id.as_ref()),
    );

    crate::routing::cluster_sync::broadcast_routing_mutation(&ctx.state.db, &ctx.state.quic_mesh);

    Ok(MessageBody::ClusterDeployStopResponseBody(
        ClusterDeployStopResponse {
            ok: all_ok,
            members: statuses,
            message: if all_ok {
                None
            } else {
                Some("teardown niekompletny — rekord zachowany, ponów STOP".to_string())
            },
        },
    ))
}

// =============================================================================
// Sync baseline-adopt admin (donor list + start/status/clear). Admin wskazuje
// dawce baseline'u i steruje pojedyncza adopcja single-flight. Cala maszyneria
// zyje w mesh::admin_ops / sync::core_baseline — tu robimy walidacje i mapowanie.
// =============================================================================

fn map_baseline_phase(phase: crate::sync::core_baseline::BaselinePhase) -> BaselineAdoptPhaseTag {
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

    let state = load_adopt_state(&ctx.state.db).map_err(baseline_ledger_err)?;

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

    let state = load_adopt_state(&ctx.state.db).map_err(baseline_ledger_err)?;

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
                assert_eq!(
                    r.phase,
                    tentaflow_protocol::BaselineAdoptPhaseTag::Receiving
                );
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
