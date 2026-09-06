// =============================================================================
// Plik: mesh/command_executor.rs
// Opis: Executor komend mesh — wykonuje komendy zarzadzania otrzymane od
//       zdalnych nodow. Sprawdza trust przed wykonaniem.
// =============================================================================

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock as AsyncRwLock;
use tracing::{debug, info, warn};
use zeroize::Zeroize;

use crate::db::DbPool;
use crate::mesh::security::MeshSecurity;
use crate::services::ports::PortAllocator;
use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType};

/// Resources required by cross-node service action handlers (krok N3b).
/// Wired up after `MeshCommandExecutor::new` once the rest of `AppState` is
/// constructed; absent in tests / when the supervisor never started, in
/// which case ServiceDeleteRemote / ServicePinRemote / ... return an error.
#[derive(Clone)]
pub struct ServiceActionContext {
    pub db: DbPool,
    pub port_allocator: Arc<PortAllocator>,
    pub iroh: Arc<crate::mesh::iroh_manager::IrohMeshManager>,
    /// Router inferencji tego węzła — używany przez `MlChat`, by odpalić model
    /// FT wdrożony lokalnie (na tym węźle) na zlecenie innego węzła mesh.
    pub router: Arc<crate::routing::router::Router>,
    /// Owning robot-control addons live here; the `RobotControl` handler resolves
    /// the local robot addon and dispatches the sanitized action through it.
    pub addon_manager: Arc<crate::addon::AddonManager>,
}

/// Odpowiedz na komende mesh — mapowana 1:1 na MeshMessage::MeshCommandResponse
pub struct CommandResponse {
    pub ok: bool,
    pub payload: MeshCommandResponsePayload,
    pub error: Option<String>,
}

impl CommandResponse {
    /// Pomocniczy konstruktor sukcesu z dowolnym payloadem.
    fn ok(payload: MeshCommandResponsePayload) -> Self {
        Self {
            ok: true,
            payload,
            error: None,
        }
    }

    /// Pomocniczy konstruktor bledu — payload Empty + komunikat.
    fn fail(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            payload: MeshCommandResponsePayload::Empty,
            error: Some(error.into()),
        }
    }
}

/// Executor komend mesh — weryfikuje trust i wykonuje komendy od zdalnych nodow.
///
/// `local_node_id` jest uzywane przez handlery profilowania do lokalizacji
/// sesji w storage (`<HOME>/profiling/<local_node_id>/<session>/`).
pub struct MeshCommandExecutor {
    security: Arc<MeshSecurity>,
    local_node_id: String,
    /// Trzymane do walidacji `validate_target_dir` (cert provisioning).
    #[allow(dead_code)]
    data_dir: PathBuf,
    /// Service-action context wired in after AppState initialisation. `None`
    /// disables ServiceDeleteRemote / ServicePinRemote / ... handlers.
    service_actions: AsyncRwLock<Option<ServiceActionContext>>,
    /// Duplicate suppression for cross-node robot commands (per from-node + actor
    /// + robot + command_id). E-stop-class commands are never cached.
    robot_idem: std::sync::Mutex<crate::mesh::robot_control::IdempotencyCache>,
    /// Per-robot async serialization of the check→execute→record critical section
    /// for NON-estop robot commands. Without it two concurrent identical commands
    /// could both miss the idempotency cache and actuate twice. The std `robot_idem`
    /// mutex only guards the brief get/record calls (never held across an await);
    /// this async lock guards the whole critical section across the addon call.
    /// E-stop-class actions BYPASS this lock entirely and execute immediately.
    robot_exec_locks: dashmap::DashMap<String, Arc<tokio::sync::Mutex<()>>>,
}

impl MeshCommandExecutor {
    pub fn new(security: Arc<MeshSecurity>, local_node_id: String, data_dir: PathBuf) -> Self {
        Self {
            security,
            local_node_id,
            data_dir,
            service_actions: AsyncRwLock::new(None),
            robot_idem: std::sync::Mutex::new(crate::mesh::robot_control::IdempotencyCache::new()),
            robot_exec_locks: dashmap::DashMap::new(),
        }
    }

    /// Inject the resources needed for cross-node service action handlers.
    /// Called once during startup after the supervisor and iroh manager are
    /// up. Subsequent calls overwrite the previous context.
    pub async fn set_service_action_context(&self, ctx: ServiceActionContext) {
        let recovery=crate::services::account_move::MoveContext{db:ctx.db.clone(),ports:ctx.port_allocator.clone(),mesh:ctx.iroh.clone(),security:self.security.clone()};
        *self.service_actions.write().await = Some(ctx);
        tokio::spawn(async move {if let Err(error)=crate::services::account_move::recover(recovery).await {tracing::error!(%error,"account transfer recovery failed");}});
    }

    async fn service_action_ctx(&self) -> Option<ServiceActionContext> {
        self.service_actions.read().await.clone()
    }

    /// Wykonaj komende od zdalnego noda. Sprawdza trust przed wykonaniem.
    pub async fn execute(&self, from_node_id: &str, command: MeshCommandType) -> CommandResponse {
        if !self.security.is_trusted(from_node_id) {
            warn!(
                from = %from_node_id,
                "Odrzucono komende od niezaufanego noda"
            );
            // Trust is also what keeps a Code Studio stream open. A peer that
            // reaches us without it must not keep a live stream on the other
            // side of the connection, and it must not keep verification keys
            // in our pool either (§12.2 — closing states the reason).
            crate::code_studio::mesh_stream::hub().close_for_node(
                from_node_id,
                crate::code_studio::mesh_stream::REASON_TRUST_LOST,
                "peer is no longer trusted",
            );
            crate::code_studio::assertion::forget_peer_keys(from_node_id);
            return CommandResponse::fail(format!("Node {} nie jest zaufany", from_node_id));
        }

        if matches!(command, MeshCommandType::ProfilingActiveInfo(_)) {
            debug!(
                from = %from_node_id,
                command = ?command,
                "Wykonuje komende mesh"
            );
        } else {
            info!(
                from = %from_node_id,
                command = ?command,
                "Wykonuje komende mesh"
            );
        }

        match command {
            MeshCommandType::ProvisionCerts {
                cert_pem,
                key_pem,
                target_dir,
            } => {
                self.handle_provision_certs(&cert_pem, &key_pem, &target_dir)
                    .await
            }

            MeshCommandType::ListContainers => {
                CommandResponse::ok(MeshCommandResponsePayload::ContainerList(Vec::new()))
            }

            MeshCommandType::ListImages => {
                CommandResponse::ok(MeshCommandResponsePayload::ImageList(Vec::new()))
            }

            MeshCommandType::AddService { .. } => CommandResponse::ok(
                MeshCommandResponsePayload::Text("Service registration queued".to_string()),
            ),

            MeshCommandType::NetworkConfig {
                interface,
                ipv4,
                netmask,
                gateway,
                dhcp,
                mut sudo_password,
                mtu,
            } => {
                // Blokujaca operacja sudo — przenies na oddzielny watek
                let iface = interface.clone();
                let ip = ipv4.clone();
                let mask = netmask.clone();
                let gw = gateway.clone();
                let mut pwd = sudo_password.clone();
                sudo_password.zeroize();
                let result = tokio::task::spawn_blocking(move || {
                    let r = crate::mesh::network_config::apply_network_config(
                        &iface,
                        ip.as_deref(),
                        mask.as_deref(),
                        gw.as_deref(),
                        dhcp,
                        mtu,
                        &pwd,
                    );
                    pwd.zeroize();
                    r
                })
                .await;
                match result {
                    Ok(Ok(output)) => CommandResponse::ok(MeshCommandResponsePayload::Text(output)),
                    Ok(Err(e)) => CommandResponse::fail(e.to_string()),
                    Err(e) => CommandResponse::fail(format!("Blad watku: {}", e)),
                }
            }

            MeshCommandType::ConfigBundleExport => match self.service_action_ctx().await {
                Some(ctx) => {
                    let local_environment =
                        crate::services::environment::get_node_environment(&ctx.db);
                    let requester_environment =
                        crate::db::repository::get_trusted_node_environment(&ctx.db, from_node_id)
                            .ok()
                            .flatten();
                    // Donor-side gate (P2-6): `environment_isolation=strict`
                    // means this node refuses to export config to a peer
                    // outside its own environment — mirrors the pairing
                    // handshake's `strict` rejection (`net::iroh::pairing::
                    // verify_request`), applied here to the donor role
                    // specifically because a config bundle is the one
                    // deliberate path that is allowed to cross environments
                    // AT ALL (everything else fences it outright).
                    let strict = crate::services::environment::is_isolation_strict(&ctx.db);
                    if strict && requester_environment != Some(local_environment) {
                        let _ = crate::db::repository::log_audit(
                            &ctx.db,
                            None,
                            None,
                            "environment.bundle_export_denied",
                            Some(&format!("node:{from_node_id}")),
                            Some(&serde_json::json!({
                                "requester_node_id": from_node_id,
                                "requester_environment": requester_environment.map(|e| e.as_str()),
                                "donor_environment": local_environment.as_str(),
                            }).to_string()),
                            None,
                            Some(&self.local_node_id),
                        );
                        return CommandResponse::fail(format!(
                            "environment isolation is strict on this node ('{}') — refusing config \
                             bundle export to peer '{}' (environment '{}')",
                            local_environment,
                            from_node_id,
                            requester_environment
                                .map(|e| e.to_string())
                                .unwrap_or_else(|| "unknown".to_string())
                        ));
                    }
                    match crate::services::config_bundle::export_bundle(
                        &ctx.db,
                        &self.local_node_id,
                    ) {
                        Ok(exported) => {
                            let table_counts = exported
                                .table_counts
                                .into_iter()
                                .map(|(table, row_count)| {
                                    tentaflow_protocol::environment::EnvironmentBundleTableCount {
                                        table,
                                        row_count,
                                    }
                                })
                                .collect();
                            let _ = crate::db::repository::log_audit(
                                &ctx.db,
                                None,
                                None,
                                "environment.bundle_exported",
                                Some(&format!("node:{from_node_id}")),
                                Some(&serde_json::json!({
                                    "requester_node_id": from_node_id,
                                    "requester_environment": requester_environment.map(|e| e.as_str()),
                                    "donor_environment": local_environment.as_str(),
                                }).to_string()),
                                None,
                                Some(&self.local_node_id),
                            );
                            CommandResponse::ok(MeshCommandResponsePayload::ConfigBundleExport {
                                archive_bytes: exported.archive_bytes,
                                filename: exported.filename,
                                manifest_sha256: exported.manifest_sha256,
                                source_environment: exported.bundle.source_environment,
                                table_counts,
                            })
                        }
                        Err(e) => {
                            CommandResponse::fail(format!("config bundle export failed: {e}"))
                        }
                    }
                }
                None => CommandResponse::fail("service action context not wired"),
            },

            MeshCommandType::RoceProbe => {
                // Enumeracja RoCE/RDMA kart noda — czysto lokalne czytanie /sys.
                let interfaces = crate::mesh::roce_config::enumerate_roce_interfaces();
                CommandResponse::ok(MeshCommandResponsePayload::RoceInterfaceList(interfaces))
            }

            MeshCommandType::ContainerStart { container_id } => {
                self.handle_container_start(&container_id).await
            }
            MeshCommandType::ContainerStop { container_id } => {
                self.handle_container_stop(&container_id).await
            }
            MeshCommandType::ContainerRestart { container_id } => {
                self.handle_container_restart(&container_id).await
            }
            MeshCommandType::SystemPrune { volumes } => self.handle_system_prune(volumes).await,

            MeshCommandType::ContainerLogs {
                container_id,
                tail_lines,
            } => self.handle_container_logs(&container_id, tail_lines).await,

            MeshCommandType::BandwidthProbe {
                target_ip,
                target_port,
                rdma_port: _,
                bind_interface,
                duration_ms,
                mode,
                nonce,
                num_streams,
            } => {
                let nonce_arr: [u8; 32] = nonce.try_into().unwrap_or([0u8; 32]);

                match mode.as_str() {
                    "server" => {
                        // Startuj TCP server ZAWSZE (fallback)
                        let tcp_result = crate::mesh::bandwidth_probe::start_probe_server(
                            &target_ip,
                            &nonce_arr,
                            num_streams,
                            duration_ms,
                        )
                        .await;

                        let (tcp_port, tcp_handle) = match tcp_result {
                            Ok((port, handle)) => (port, Some(handle)),
                            Err(e) => {
                                return CommandResponse::fail(format!("TCP server failed: {}", e));
                            }
                        };

                        // Server negotiates its own RDMA listener port locally; it's a different
                        // value from the caller-supplied `rdma_port` (which is a client-side hint).
                        // Mutacja tylko w cfg(rdma-probe); bez tego feature'u `mut` jest nieuzywany.
                        #[allow(unused_mut)]
                        let mut server_rdma_port: u16 = 0;
                        #[cfg(feature = "rdma-probe")]
                        if let Some(rdma_dev) =
                            crate::mesh::rdma_probe::find_rdma_device_for_interface(&bind_interface)
                        {
                            match crate::mesh::rdma_probe::start_rdma_probe_server(
                                &target_ip,
                                &rdma_dev,
                                &nonce_arr,
                                duration_ms,
                            )
                            .await
                            {
                                Ok((port, handle)) => {
                                    server_rdma_port = port;
                                    tokio::spawn(async move {
                                        let _ = handle.await;
                                    });
                                    tracing::info!("RDMA server na porcie {}", port);
                                }
                                Err(e) => {
                                    tracing::warn!("RDMA server probe failed: {}", e);
                                }
                            }
                        }

                        // Spawn TCP handle w tle
                        if let Some(handle) = tcp_handle {
                            tokio::spawn(async move {
                                let _ = handle.await;
                            });
                        }

                        // Zwroc OBA porty — klient sprobuje RDMA, jesli fail uzyje TCP
                        CommandResponse::ok(
                            MeshCommandResponsePayload::BandwidthProbeServerStarted {
                                tcp_port,
                                rdma_port: server_rdma_port,
                            },
                        )
                    }
                    "client" => {
                        // Probuj RDMA jesli serwer zwrocil rdma_port > 0
                        #[cfg(feature = "rdma-probe")]
                        if rdma_port > 0 {
                            if let Some(rdma_dev) =
                                crate::mesh::rdma_probe::find_rdma_device_for_interface(
                                    &bind_interface,
                                )
                            {
                                match crate::mesh::rdma_probe::start_rdma_probe_client(
                                    &target_ip,
                                    rdma_port,
                                    &rdma_dev,
                                    &nonce_arr,
                                    duration_ms,
                                )
                                .await
                                {
                                    Ok(result) => {
                                        return CommandResponse::ok(
                                            MeshCommandResponsePayload::BandwidthProbeClientResult {
                                                bandwidth_mbps: result.bandwidth_mbps,
                                                bytes_transferred: result.bytes_transferred,
                                                duration_ms: result.duration_ms,
                                                latency_us: result.latency_us,
                                                streams_completed: 1,
                                                rdma: true,
                                            },
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!("RDMA client failed, fallback TCP: {}", e);
                                    }
                                }
                            }
                        }

                        // TCP multi-stream (fallback lub jedyny tryb)
                        match crate::mesh::bandwidth_probe::start_probe_client(
                            &target_ip,
                            target_port,
                            &bind_interface,
                            &nonce_arr,
                            num_streams,
                            duration_ms,
                        )
                        .await
                        {
                            Ok(result) => CommandResponse::ok(
                                MeshCommandResponsePayload::BandwidthProbeClientResult {
                                    bandwidth_mbps: result.bandwidth_mbps,
                                    bytes_transferred: result.bytes_transferred,
                                    duration_ms: result.duration_ms,
                                    latency_us: result.latency_us,
                                    streams_completed: result.streams_completed,
                                    rdma: false,
                                },
                            ),
                            Err(e) => CommandResponse::fail(e.to_string()),
                        }
                    }
                    _ => CommandResponse::fail("Nieznany tryb probing"),
                }
            }

            MeshCommandType::BandwidthProbeCancel => {
                CommandResponse::ok(MeshCommandResponsePayload::Empty)
            }

            MeshCommandType::ProfilingStart(req) => self.handle_profiling_start(req).await,
            MeshCommandType::ProfilingStop(req) => self.handle_profiling_stop(req).await,
            MeshCommandType::ProfilingSessions(req) => self.handle_profiling_sessions(req).await,
            MeshCommandType::ProfilingReport(req) => self.handle_profiling_report(req).await,
            MeshCommandType::ProfilingDelete(req) => self.handle_profiling_delete(req).await,
            MeshCommandType::ProfilingDownload(req) => self.handle_profiling_download(req).await,
            MeshCommandType::ProfilingActiveInfo(req) => {
                self.handle_profiling_active_info(req).await
            }

            MeshCommandType::ServiceStartRemote { service_id } => {
                self.handle_service_start_remote(service_id).await
            }
            MeshCommandType::ServiceDeleteRemote { service_id } => {
                self.handle_service_delete_remote(service_id).await
            }
            MeshCommandType::ServicePinRemote { service_id, pinned } => {
                self.handle_service_pin_remote(service_id, pinned).await
            }
            MeshCommandType::ServicePauseRemote { service_id, paused } => {
                self.handle_service_pause_remote(service_id, paused).await
            }
            MeshCommandType::ServiceDeployRemote {
                engine_id,
                deploy_method,
                config_json,
            } => {
                self.handle_service_deploy_remote(
                    from_node_id,
                    &engine_id,
                    &deploy_method,
                    &config_json,
                )
                .await
            }
            MeshCommandType::ServiceDeployDistributed { spec } => {
                self.handle_service_deploy_distributed(from_node_id, spec)
                    .await
            }
            MeshCommandType::ServiceStopDistributed {
                deployment_cluster_id,
            } => {
                self.handle_service_stop_distributed(&deployment_cluster_id)
                    .await
            }
            MeshCommandType::DistributedReadiness {
                deployment_cluster_id,
                ray_port,
                serve_port,
                expected_nodes: _,
            } => {
                let st = crate::services::deploy::distributed::probe_readiness(
                    &deployment_cluster_id,
                    ray_port,
                    serve_port,
                )
                .await;
                CommandResponse::ok(MeshCommandResponsePayload::DistributedReadinessResult {
                    container_running: st.container_running,
                    ray_gcs_up: st.ray_gcs_up,
                    ray_nodes: st.ray_nodes,
                    serve_ready: st.serve_ready,
                    error: st.error,
                })
            }
            MeshCommandType::DistributedStartServe {
                deployment_cluster_id,
                serve_cmd,
            } => {
                match crate::services::deploy::distributed::exec_serve_on_head(
                    &deployment_cluster_id,
                    &serve_cmd,
                )
                .await
                {
                    Ok(()) => CommandResponse::ok(MeshCommandResponsePayload::Empty),
                    Err(e) => CommandResponse::fail(e),
                }
            }
            MeshCommandType::ServiceUpdateRemote {
                service_id,
                model_repo,
                model_preset_id,
                gpu_memory_utilization,
                max_model_len,
                max_num_seqs,
                max_num_batched_tokens,
                kv_cache_dtype,
                chunked_prefill,
                vllm_args_override,
                pinned,
                paused,
                restart_after_save,
            } => {
                self.handle_service_update_remote(
                    service_id,
                    model_repo,
                    model_preset_id,
                    gpu_memory_utilization,
                    max_model_len,
                    max_num_seqs,
                    max_num_batched_tokens,
                    kv_cache_dtype,
                    chunked_prefill,
                    vllm_args_override,
                    pinned,
                    paused,
                    restart_after_save,
                )
                .await
            }
            MeshCommandType::WebResearch { request_json } => {
                self.handle_web_research(request_json).await
            }
            MeshCommandType::VectorOp { request_cbor } => self.handle_vector_op(request_cbor).await,
            MeshCommandType::OauthStart { provider } => self.handle_oauth_start(provider).await,
            MeshCommandType::OauthPoll { flow_id } => self.handle_oauth_poll(flow_id).await,
            MeshCommandType::AgentAccountMove { operation,payload_json } => {
                let Some(actions)=self.service_action_ctx().await else {return CommandResponse::fail("service action context unavailable");};
                let context=crate::services::account_move::MoveContext{db:actions.db,ports:actions.port_allocator,mesh:actions.iroh,security:self.security.clone()};
                match crate::services::account_move::receive(&context,from_node_id,&operation,&payload_json).await {
                    Ok(result_json)=>CommandResponse::ok(MeshCommandResponsePayload::AgentRpcResult{result_json}),
                    Err(error)=>CommandResponse::fail(error.to_string()),
                }
            }
            MeshCommandType::AgentRpc {
                service_id,
                operation,
                payload_json,
                user_id,
            } => {
                self.handle_agent_rpc(service_id, operation, payload_json, user_id)
                    .await
            }

            MeshCommandType::CodeStudioOp {
                assertion,
                payload_cbor,
            } => {
                self.handle_code_studio_op(from_node_id, assertion, payload_cbor)
                    .await
            }
            MeshCommandType::AppRouteOp {
                assertion,
                payload_cbor,
            } => {
                self.handle_app_route_op(from_node_id, assertion, payload_cbor)
                    .await
            }
            MeshCommandType::CodeStudioAssertionKeysPush { keys } => {
                let accepted = crate::code_studio::assertion::ingest_peer_keys(from_node_id, &keys);
                debug!(
                    from = %from_node_id,
                    accepted,
                    "code studio: assertion keys ingested"
                );
                CommandResponse::ok(MeshCommandResponsePayload::Empty)
            }
            MeshCommandType::CodeStudioAssertionKeysGet => {
                CommandResponse::ok(MeshCommandResponsePayload::CodeStudioAssertionKeysResult {
                    keys: crate::code_studio::assertion::local_advertise(),
                })
            }
            MeshCommandType::CodeStudioPermissionProbe {
                user_id,
                org_id,
                workspace_id,
                capability,
            } => {
                self.handle_code_studio_permission_probe(user_id, org_id, workspace_id, capability)
                    .await
            }
            MeshCommandType::CodeStudioStreamPull {
                assertion,
                request_cbor,
            } => {
                self.handle_code_studio_stream_pull(from_node_id, assertion, request_cbor)
                    .await
            }
            MeshCommandType::CodeStudioStreamOpen {
                assertion,
                request_cbor,
            } => {
                self.handle_code_studio_stream_open(from_node_id, assertion, request_cbor)
                    .await
            }
            MeshCommandType::MlTrainStart { run_id, spec_json } => {
                self.handle_ml_train_start(run_id, spec_json).await
            }
            MeshCommandType::MlTrainStatus { run_id } => self.handle_ml_train_status(run_id).await,
            MeshCommandType::MlTrainCancel { run_id } => self.handle_ml_train_cancel(run_id).await,
            MeshCommandType::MlDatasetChunk {
                dataset_hash,
                seq,
                total,
                data_b64,
            } => {
                self.handle_ml_dataset_chunk(dataset_hash, seq, total, data_b64)
                    .await
            }
            MeshCommandType::MlDetect {
                checkpoint_path,
                class_names_json,
                variant,
                threshold,
                image_b64,
            } => {
                self.handle_ml_detect(
                    checkpoint_path,
                    class_names_json,
                    variant,
                    threshold,
                    image_b64,
                )
                .await
            }
            MeshCommandType::MlExport {
                export_id,
                spec_json,
            } => {
                match crate::ml_studio::export_llm::mesh_export_start(&export_id, &spec_json).await
                {
                    Ok(()) => CommandResponse::ok(MeshCommandResponsePayload::Empty),
                    Err(e) => CommandResponse::fail(format!("mesh export start: {}", e)),
                }
            }
            MeshCommandType::MlExportStatus { export_id } => {
                match crate::ml_studio::export_llm::mesh_export_status(&export_id).await {
                    Ok(status_json) => {
                        CommandResponse::ok(MeshCommandResponsePayload::MlExportStatusResult {
                            status_json,
                        })
                    }
                    Err(e) => CommandResponse::fail(format!("mesh export status: {}", e)),
                }
            }
            MeshCommandType::MlChat {
                model_name,
                message,
                max_tokens,
            } => self.handle_ml_chat(model_name, message, max_tokens).await,
            MeshCommandType::MlArtifactPushTo {
                src_path,
                target_node_id,
            } => {
                self.handle_ml_artifact_push_to(src_path, target_node_id)
                    .await
            }
            MeshCommandType::RobotControl { request_cbor } => {
                self.handle_robot_control(from_node_id, request_cbor).await
            }
            MeshCommandType::EnsureModelLocal {
                deployment_cluster_id,
                model_repo,
                engine_id,
            } => {
                self.handle_ensure_model_local(
                    from_node_id,
                    deployment_cluster_id,
                    model_repo,
                    engine_id,
                )
                .await
            }
            MeshCommandType::ModelPresentLocal {
                deployment_cluster_id,
                model_repo,
            } => {
                if let Err(e) =
                    self.authorize_cluster_deploy_peer(from_node_id, &deployment_cluster_id)
                {
                    return CommandResponse::fail(e);
                }
                let present =
                    crate::services::deploy::distributed::model_snapshot_dir(&model_repo).is_some();
                CommandResponse::ok(MeshCommandResponsePayload::ModelPresentResult { present })
            }
            MeshCommandType::PushModelToPeer {
                deployment_cluster_id,
                model_repo,
                target_node_id,
            } => {
                self.handle_push_model_to_peer(
                    from_node_id,
                    deployment_cluster_id,
                    model_repo,
                    target_node_id,
                )
                .await
            }
            MeshCommandType::CameraRecordingsList { filters_json } => {
                self.handle_camera_recordings_list(filters_json).await
            }
            MeshCommandType::CameraRecordingPull {
                recording_refs,
                target_node_id,
            } => {
                self.handle_camera_recording_pull(recording_refs, target_node_id)
                    .await
            }
        }
    }

    /// Owner side (węzeł-źródło): pakuje katalog artefaktu i streamuje go do
    /// `target_node_id` przez mesh. Wymaga `iroh` z service-action context.
    async fn handle_ml_artifact_push_to(
        &self,
        src_path: String,
        target_node_id: String,
    ) -> CommandResponse {
        // Fail-closed: węzeł docelowy musi być zaufany lokalnie, a `src_path` musi
        // być legalnym katalogiem artefaktu ML Studio (nie dowolny katalog na dysku).
        if !self.security.is_trusted(&target_node_id) {
            return CommandResponse::fail(format!("target {} nie jest zaufany", target_node_id));
        }
        if !crate::ml_studio::mesh_artifact::is_allowed_artifact_dir(&src_path) {
            return CommandResponse::fail(format!(
                "src_path nie jest dozwolonym katalogiem artefaktu: {}",
                src_path
            ));
        }
        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("service action context not configured");
        };
        match crate::ml_studio::mesh_artifact::push_dir_to(
            &ctx.iroh,
            &target_node_id,
            &src_path,
            None,
        )
        .await
        {
            Ok(target_path) => {
                CommandResponse::ok(MeshCommandResponsePayload::MlArtifactPushResult {
                    target_path,
                    error: None,
                })
            }
            Err(e) => CommandResponse::ok(MeshCommandResponsePayload::MlArtifactPushResult {
                target_path: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }

    /// B side: list THIS node's recordings for a paired puller. Resolves the local
    /// org context (`None` → default org, exactly like the local recordings-list
    /// handler) and applies the JSON-carried filters. Recordings are node-global;
    /// the org boundary still scopes the rows.
    #[cfg(feature = "camera")]
    async fn handle_camera_recordings_list(&self, filters_json: String) -> CommandResponse {
        use crate::mesh::recordings_pull::{RemoteRecordingFilters, RemoteRecordingItem};
        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("recordings list context is not initialized");
        };
        let filters = match serde_json::from_str::<RemoteRecordingFilters>(&filters_json) {
            Ok(f) => f,
            Err(e) => return CommandResponse::fail(format!("invalid recordings filters: {e}")),
        };
        let db = ctx.db.clone();
        let result = tokio::task::spawn_blocking(move || {
            let limit = filters.limit.clamp(1, 1000);
            let camera_id = filters
                .camera_id
                .as_deref()
                .filter(|s| !s.trim().is_empty());
            // Wire timestamps are unix MILLISECONDS; the recordings table is SECONDS.
            let created_from = filters.date_from_ms.map(|ms| ms / 1000);
            let created_to = filters.date_to_ms.map(|ms| ms / 1000);
            let repo_filters = crate::db::repository::RecordingListFilters {
                owner_addon_id: None,
                kind: None,
                camera_id,
                created_from,
                created_to,
                plate: None,
                adr: None,
            };
            crate::db::repository::list_recordings(&db, None, &repo_filters, limit)
        })
        .await;
        let rows = match result {
            Ok(Ok(rows)) => rows,
            Ok(Err(e)) => return CommandResponse::fail(format!("list recordings: {e}")),
            Err(e) => return CommandResponse::fail(format!("recordings list task failed: {e}")),
        };
        let items: Vec<RemoteRecordingItem> = rows
            .into_iter()
            .map(|r| RemoteRecordingItem {
                recording_ref: r.recording_ref,
                kind: r.kind,
                camera_id: r.camera_id,
                created_at_ms: r.created_at.saturating_mul(1000),
                duration_ms: r.duration_ms,
                file_size_bytes: r.file_size_bytes,
                plate_text: r.plate_text,
                adr_text: r.adr_text,
            })
            .collect();
        match serde_json::to_string(&items) {
            Ok(recordings_json) => {
                CommandResponse::ok(MeshCommandResponsePayload::CameraRecordingsListResult {
                    recordings_json,
                })
            }
            Err(e) => CommandResponse::fail(format!("serialize recordings list: {e}")),
        }
    }

    #[cfg(not(feature = "camera"))]
    async fn handle_camera_recordings_list(&self, _filters_json: String) -> CommandResponse {
        CommandResponse::fail("recordings require the camera module")
    }

    /// B side: stream the requested recording files to `target_node_id` over
    /// ALPN_ARTIFACT. Each `file_path` is re-validated for containment (never
    /// trusting the DB path), a per-file size cap protects the puller's disk, and
    /// a max ref count bounds the number of streams. Returns the refs actually
    /// streamed.
    #[cfg(feature = "camera")]
    async fn handle_camera_recording_pull(
        &self,
        recording_refs: Vec<String>,
        target_node_id: String,
    ) -> CommandResponse {
        use crate::mesh::recordings_pull::MAX_REFS_PER_PULL;
        use crate::ml_studio::mesh_artifact::MAX_RECORDING_BYTES;
        if !self.security.is_trusted(&target_node_id) {
            return CommandResponse::fail(format!("target {target_node_id} nie jest zaufany"));
        }
        if recording_refs.len() > MAX_REFS_PER_PULL {
            return CommandResponse::fail(format!(
                "pull requests {} recordings, max is {}",
                recording_refs.len(),
                MAX_REFS_PER_PULL
            ));
        }
        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("recordings pull context is not initialized");
        };
        let mut pulled_refs = Vec::with_capacity(recording_refs.len());
        for rec_ref in &recording_refs {
            let db = ctx.db.clone();
            let ref_for_task = rec_ref.clone();
            let row = tokio::task::spawn_blocking(move || {
                crate::db::repository::get_recording_by_ref(&db, &ref_for_task)
            })
            .await;
            let file_path = match row {
                Ok(Ok(Some(r))) => r.file_path,
                Ok(Ok(None)) => {
                    warn!(recording = %rec_ref, "recordings pull: unknown ref — skipping");
                    continue;
                }
                Ok(Err(e)) => {
                    warn!(recording = %rec_ref, "recordings pull: lookup failed: {e}");
                    continue;
                }
                Err(e) => {
                    warn!(recording = %rec_ref, "recordings pull: lookup task failed: {e}");
                    continue;
                }
            };
            let canonical = match crate::mesh::recordings_pull::validate_local_recording_path(
                &file_path,
                MAX_RECORDING_BYTES as i64,
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    warn!(recording = %rec_ref, "recordings pull: path rejected: {e}");
                    continue;
                }
            };
            match crate::ml_studio::mesh_artifact::push_recording_to(
                &ctx.iroh,
                &target_node_id,
                rec_ref,
                &canonical,
            )
            .await
            {
                Ok(()) => pulled_refs.push(rec_ref.clone()),
                Err(e) => {
                    warn!(recording = %rec_ref, "recordings pull: stream failed: {e}")
                }
            }
        }
        CommandResponse::ok(MeshCommandResponsePayload::CameraRecordingPullResult { pulled_refs })
    }

    #[cfg(not(feature = "camera"))]
    async fn handle_camera_recording_pull(
        &self,
        _recording_refs: Vec<String>,
        _target_node_id: String,
    ) -> CommandResponse {
        CommandResponse::fail("recordings require the camera module")
    }

    /// Autoryzacja komendy P0 cluster-deploy (EnsureModelLocal / ModelPresentLocal /
    /// PushModelToPeer). Rekord deploymentu (`cluster_deployments`) zyje WYLACZNIE w
    /// DB koordynatora — NIGDY nie jest synchronizowany przez mesh, wiec odbiorca nie
    /// moze rozwiazac `deployment_cluster_id` lokalnie. Zamiast tego dowiazujemy do
    /// SYNCHRONIZOWANEJ przynaleznosci klastra (`cluster_members`, replikowana przez
    /// `MESH_MSG_ROUTING_SYNC`): nadawca komendy (koordynator/head deployu) ORAZ TEN
    /// wezel musza byc czlonkami jednego wspolnego klastra. To zamienia „dowolny
    /// zaufany peer" w „peer, ktory dzieli z nami klaster" — najsilniejsze wiazanie
    /// weryfikowalne po stronie odbiorcy. `deployment_cluster_id` jest logowany do
    /// korelacji z deployem. Zwraca `cluster_id` wspolnego klastra.
    fn authorize_cluster_deploy_peer(
        &self,
        from_node_id: &str,
        deployment_cluster_id: &str,
    ) -> Result<String, String> {
        let all = crate::db::repository::list_all_cluster_members(&self.security.db)
            .map_err(|e| format!("odczyt czlonkow klastra: {e}"))?;
        // Koordynator (nadawca) moze orkiestrowac deploy z wezla SPOZA compute-clustra
        // (trigger z dowolnego zaufanego admin-node) — jego node_id nie musi byc w
        // `cluster_members`. Zaufanie nadawcy jest juz wymuszone przez warstwe mesh
        // (komenda nie dotarlaby do execute od niesparowanego peera). Po stronie
        // odbiorcy weryfikujemy to, co ma sens: TEN wezel jest czlonkiem klastra
        // (operacje P0 na modelu dotycza tylko wezlow klastra). Zwracany cluster_id
        // ogranicza cel `PushModelToPeer` do wspolczlonkow tego samego klastra.
        let my_cluster = all
            .iter()
            .find(|m| m.node_id == self.local_node_id)
            .map(|m| m.cluster_id.clone());
        match my_cluster {
            Some(cluster_id) => {
                debug!(
                    from = %from_node_id,
                    deployment = %deployment_cluster_id,
                    cluster = %cluster_id,
                    "autoryzacja komendy P0 cluster-deploy OK (odbiorca jest czlonkiem klastra)"
                );
                Ok(cluster_id)
            }
            None => {
                warn!(
                    from = %from_node_id,
                    deployment = %deployment_cluster_id,
                    "odrzucono komende P0 cluster-deploy: ten wezel nie jest czlonkiem zadnego klastra"
                );
                Err("unauthorized: this node is not a cluster member".to_string())
            }
        }
    }

    /// Czy `node_id` jest czlonkiem klastra `cluster_id` (wg synchronizowanej
    /// przynaleznosci). Uzywane przez `PushModelToPeer`, zeby ograniczyc cel
    /// transferu do czlonkow tego samego klastra co nadawca.
    fn node_in_cluster(&self, node_id: &str, cluster_id: &str) -> bool {
        crate::db::repository::list_cluster_members(&self.security.db, cluster_id)
            .map(|members| members.iter().any(|m| m.node_id == node_id))
            .unwrap_or(false)
    }

    /// P0 cluster deploy (odbiorca = head zdalny): upewnia się, że model jest w
    /// lokalnym cache HF; pobiera go jeśli brak. `HF_TOKEN` bierzemy z WŁASNEGO
    /// secure setting tego węzła — token nigdy nie leci przez mesh.
    async fn handle_ensure_model_local(
        &self,
        from_node_id: &str,
        deployment_cluster_id: String,
        model_repo: String,
        engine_id: String,
    ) -> CommandResponse {
        if let Err(e) = self.authorize_cluster_deploy_peer(from_node_id, &deployment_cluster_id) {
            return CommandResponse::fail(e);
        }
        let hf_token = crate::db::repository::get_setting_secure(
            &self.security.db,
            "hf_token",
            self.security.settings_cipher(),
        )
        .ok()
        .flatten()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
        match crate::services::deploy::distributed::ensure_model_downloaded_local(
            &model_repo,
            &engine_id,
            hf_token.as_deref(),
            &deployment_cluster_id,
        )
        .await
        {
            Ok(dir) => CommandResponse::ok(MeshCommandResponsePayload::EnsureModelResult {
                snapshot_dir: dir.to_string_lossy().to_string(),
                error: None,
            }),
            Err(e) => CommandResponse::ok(MeshCommandResponsePayload::EnsureModelResult {
                snapshot_dir: String::new(),
                error: Some(e),
            }),
        }
    }

    /// P0 cluster deploy (odbiorca = head zdalny): pakuje snapshot modelu z
    /// lokalnego cache HF i streamuje go do `target_node_id` (worker), który zapisze
    /// go do swojego cache HF. Fail-closed: target musi być zaufany, model obecny.
    async fn handle_push_model_to_peer(
        &self,
        from_node_id: &str,
        deployment_cluster_id: String,
        model_repo: String,
        target_node_id: String,
    ) -> CommandResponse {
        let cluster_id =
            match self.authorize_cluster_deploy_peer(from_node_id, &deployment_cluster_id) {
                Ok(id) => id,
                Err(e) => return CommandResponse::fail(e),
            };
        if !self.security.is_trusted(&target_node_id) {
            return CommandResponse::fail(format!("target {} nie jest zaufany", target_node_id));
        }
        // Cel transferu musi nalezec do TEGO SAMEGO klastra co nadawca — nie pozwalamy
        // pchnac modelu do dowolnego zaufanego wezla poza deployem.
        if !self.node_in_cluster(&target_node_id, &cluster_id) {
            return CommandResponse::fail(format!(
                "target {} nie jest czlonkiem klastra {}",
                target_node_id, cluster_id
            ));
        }
        let Some(snap) = crate::services::deploy::distributed::model_snapshot_dir(&model_repo)
        else {
            return CommandResponse::fail(format!(
                "model {} nie jest w cache tego węzła",
                model_repo
            ));
        };
        let hash = snap
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("service action context not configured");
        };
        match crate::ml_studio::mesh_artifact::push_hf_model_to(
            &ctx.iroh,
            &target_node_id,
            &model_repo,
            &snap.to_string_lossy(),
            &hash,
            None,
        )
        .await
        {
            Ok(target_path) => {
                CommandResponse::ok(MeshCommandResponsePayload::MlArtifactPushResult {
                    target_path,
                    error: None,
                })
            }
            Err(e) => CommandResponse::ok(MeshCommandResponsePayload::MlArtifactPushResult {
                target_path: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }

    /// Owner side: zapytanie do modelu FT wdrożonego NA TYM węźle (alias w lokalnym
    /// routingu). Inicjator (Node A) przysyła `model_name` + tekst; odpalamy
    /// inferencję lokalnym Routerem i zwracamy odpowiedź. Pozwala UŻYĆ z A modelu z B.
    async fn handle_ml_chat(
        &self,
        model_name: String,
        message: String,
        max_tokens: u32,
    ) -> CommandResponse {
        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("service action context not configured");
        };
        match crate::ml_studio::infer::run_local_chat(
            &ctx.router,
            &model_name,
            &message,
            max_tokens,
        )
        .await
        {
            Ok(answer) => CommandResponse::ok(MeshCommandResponsePayload::MlChatResult {
                answer,
                error: None,
            }),
            Err(e) => CommandResponse::ok(MeshCommandResponsePayload::MlChatResult {
                answer: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }

    /// Owner side: detekcja NA TYM nodzie modelem, którego checkpoint żyje tutaj
    /// (np. wytrenowanym lokalnie). Inicjator (Node A) wysyła checkpoint + klasy +
    /// obraz; my wołamy lokalny serwis i zwracamy wynik. Umożliwia testowanie z A.
    async fn handle_ml_detect(
        &self,
        checkpoint_path: String,
        class_names_json: String,
        variant: String,
        threshold: f64,
        image_b64: String,
    ) -> CommandResponse {
        let class_names: Vec<String> = serde_json::from_str(&class_names_json).unwrap_or_default();
        match crate::ml_studio::train_recognition::run_detect(
            checkpoint_path,
            class_names,
            variant,
            threshold,
            image_b64,
        )
        .await
        {
            Ok((detections_json, width, height)) => {
                CommandResponse::ok(MeshCommandResponsePayload::MlDetectResult {
                    detections_json,
                    width,
                    height,
                    error: None,
                })
            }
            Err(e) => CommandResponse::ok(MeshCommandResponsePayload::MlDetectResult {
                detections_json: "[]".to_string(),
                width: 0,
                height: 0,
                error: Some(e.to_string()),
            }),
        }
    }

    /// Owner side: przyjmuje chunk datasetu COCO (zip) i składa go do cache pod
    /// hashem. Dedup: gdy dataset (hash) już jest, zwraca have_already=true.
    async fn handle_ml_dataset_chunk(
        &self,
        dataset_hash: String,
        seq: u32,
        total: u32,
        data_b64: String,
    ) -> CommandResponse {
        match crate::ml_studio::train_recognition::mesh_dataset_chunk(
            &dataset_hash,
            seq,
            total,
            &data_b64,
        ) {
            Ok(have_already) => {
                CommandResponse::ok(MeshCommandResponsePayload::MlDatasetChunkResult {
                    have_already,
                })
            }
            Err(e) => CommandResponse::fail(format!("mesh dataset chunk: {}", e)),
        }
    }

    /// Owner side: uruchamia trening NA TYM nodzie przez lokalny serwis treningowy
    /// (ML Studio mesh-distributed). `run_id` jest kluczem śledzenia po stronie
    /// odbiorcy; inicjator (Node A) odpytuje przez `MlTrainStatus`.
    async fn handle_ml_train_start(&self, run_id: String, spec_json: String) -> CommandResponse {
        // spec.kind rozróżnia tor: "llm" → ml-training, inaczej recognition (rfdetr).
        let kind = serde_json::from_str::<serde_json::Value>(&spec_json)
            .ok()
            .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(String::from))
            .unwrap_or_default();
        let res = if kind == "llm" {
            crate::ml_studio::train_llm::mesh_train_start_llm(&run_id, &spec_json).await
        } else if kind == "classifier" {
            crate::ml_studio::train_classifier::mesh_train_start_classifier(&run_id, &spec_json)
                .await
        } else if kind == "ocr" {
            crate::ml_studio::train_ocr::mesh_train_start_ocr(&run_id, &spec_json).await
        } else {
            crate::ml_studio::train_recognition::mesh_train_start(&run_id, &spec_json).await
        };
        match res {
            Ok(()) => CommandResponse::ok(MeshCommandResponsePayload::Empty),
            Err(e) => CommandResponse::fail(format!("mesh train start: {}", e)),
        }
    }

    async fn handle_ml_train_status(&self, run_id: String) -> CommandResponse {
        // Router statusu: jeśli to job LLM zlecony tu przez mesh → ml-training,
        // inaczej recognition. Jeden run_id istnieje tylko w jednym z rejestrów.
        let res = if crate::ml_studio::train_llm::is_llm_mesh_job(&run_id) {
            crate::ml_studio::train_llm::mesh_train_status_llm(&run_id).await
        } else if crate::ml_studio::train_classifier::is_classifier_mesh_job(&run_id) {
            crate::ml_studio::train_classifier::mesh_train_status_classifier(&run_id).await
        } else if crate::ml_studio::train_ocr::is_ocr_mesh_job(&run_id) {
            crate::ml_studio::train_ocr::mesh_train_status_ocr(&run_id).await
        } else {
            crate::ml_studio::train_recognition::mesh_train_status(&run_id).await
        };
        match res {
            Ok(status_json) => {
                CommandResponse::ok(MeshCommandResponsePayload::MlTrainStatusResult { status_json })
            }
            Err(e) => CommandResponse::fail(format!("mesh train status: {}", e)),
        }
    }

    /// Owner side: anuluje trening zlecony tu przez mesh. Ten sam router rejestrów
    /// co status — jeden `run_id` istnieje w dokładnie jednym z nich.
    async fn handle_ml_train_cancel(&self, run_id: String) -> CommandResponse {
        let res = if crate::ml_studio::train_llm::is_llm_mesh_job(&run_id) {
            crate::ml_studio::train_llm::mesh_train_cancel_llm(&run_id).await
        } else if crate::ml_studio::train_classifier::is_classifier_mesh_job(&run_id) {
            crate::ml_studio::train_classifier::mesh_train_cancel_classifier(&run_id).await
        } else if crate::ml_studio::train_ocr::is_ocr_mesh_job(&run_id) {
            crate::ml_studio::train_ocr::mesh_train_cancel_ocr(&run_id).await
        } else {
            crate::ml_studio::train_recognition::mesh_train_cancel(&run_id).await
        };
        match res {
            Ok(()) => CommandResponse::ok(MeshCommandResponsePayload::Empty),
            Err(e) => CommandResponse::fail(format!("mesh train cancel: {}", e)),
        }
    }

    /// Owner side of a forwarded subscription OAuth start: run the device-code
    /// flow on THIS node (it owns the service + tokens) and return the URL/code.
    async fn handle_oauth_start(&self, provider: String) -> CommandResponse {
        if !provider.eq_ignore_ascii_case("openai") {
            return CommandResponse::ok(MeshCommandResponsePayload::OauthStartResult {
                flow_id: String::new(),
                authorize_url: String::new(),
                user_code: String::new(),
                error: Some("subscription login is only available for OpenAI".to_string()),
            });
        }
        match crate::services::backend::codex_oauth::start_login().await {
            Ok((flow_id, authorize_url, user_code)) => {
                CommandResponse::ok(MeshCommandResponsePayload::OauthStartResult {
                    flow_id,
                    authorize_url,
                    user_code,
                    error: None,
                })
            }
            Err(e) => CommandResponse::ok(MeshCommandResponsePayload::OauthStartResult {
                flow_id: String::new(),
                authorize_url: String::new(),
                user_code: String::new(),
                error: Some(e),
            }),
        }
    }

    /// Owner side of a forwarded subscription OAuth poll.
    async fn handle_oauth_poll(&self, flow_id: String) -> CommandResponse {
        let (status, account_label, error) = crate::services::backend::codex_oauth::poll(&flow_id);
        CommandResponse::ok(MeshCommandResponsePayload::OauthPollResult {
            status,
            account_label,
            error,
        })
    }

    async fn handle_agent_rpc(
        &self,
        service_id: i64,
        operation: String,
        payload_json: String,
        user_id: String,
    ) -> CommandResponse {
        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("coding-agent service context is not initialized");
        };
        if matches!(operation.as_str(),"account.move"|"account.move.status") {
            let context=crate::services::account_move::MoveContext{db:ctx.db.clone(),ports:ctx.port_allocator.clone(),mesh:ctx.iroh.clone(),security:self.security.clone()};
            return match crate::services::account_move::operate(context,service_id,&user_id,&operation,&payload_json).await {
                Ok(result_json)=>CommandResponse::ok(MeshCommandResponsePayload::AgentRpcResult{result_json}),
                Err(error)=>CommandResponse::fail(error.to_string()),
            };
        }
        let service = {
            let conn = match ctx.db.read() {
                Ok(conn) => conn,
                Err(_) => return CommandResponse::fail("database pool is poisoned"),
            };
            match crate::services_repo::services::get(&conn, service_id) {
                Ok(Some(service)) => service,
                Ok(None) => {
                    return CommandResponse::fail(format!("service id={service_id} not found"))
                }
                Err(error) => return CommandResponse::fail(error.to_string()),
            }
        };
        match crate::services::coding_agent::execute_public(&ctx.db, &service, &user_id, &operation, &payload_json).await {
            Ok(result_json) => {
                if operation == "models.list" {
                    if let Err(error) =
                        crate::services::coding_agent::sync_models(&ctx.db, &service, &result_json)
                    {
                        return CommandResponse::fail(error);
                    }
                }
                CommandResponse::ok(MeshCommandResponsePayload::AgentRpcResult { result_json })
            }
            Err(error) => CommandResponse::fail(error),
        }
    }

    /// Owner side of a forwarded Code Studio request (§12.1).
    ///
    /// Everything of substance lives in `code_studio::remote_proxy`: verify the
    /// assertion, authorize FROM SCRATCH against this node's own state, probe
    /// the issuer for permission freshness when the operation is irreversible,
    /// and then run the ordinary local handler. There is no second
    /// implementation of Code Studio behaviour here.
    ///
    /// A refusal travels as a successful mesh response carrying the typed
    /// `ProtocolError`, the same shape `RobotControl` uses: the caller must see
    /// the protocol code, not a transport failure that flattens it to a string.
    async fn handle_code_studio_op(
        &self,
        from_node_id: &str,
        assertion: tentaflow_protocol::mesh::SessionAssertion,
        payload_cbor: Vec<u8>,
    ) -> CommandResponse {
        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("code studio mesh context is not initialized");
        };
        let (payload_cbor, error) = crate::code_studio::remote_proxy::execute_owner_side(
            from_node_id,
            &assertion,
            &payload_cbor,
            &ctx.iroh,
        )
        .await;
        if let Some(error) = &error {
            warn!(
                from = %from_node_id,
                user = %assertion.sub,
                workspace = %assertion.workspace,
                code = ?error.code,
                "code studio: forwarded operation refused"
            );
        }
        CommandResponse::ok(MeshCommandResponsePayload::CodeStudioOpResult {
            payload_cbor,
            error,
        })
    }

    /// Execute one forwarded app-family dashboard request (plan §3.1). The
    /// proxy module verifies the assertion, rebuilds the actor's context from
    /// LOCAL state and runs the ordinary dispatch pipeline — this fn is only
    /// the mesh plumbing around it.
    async fn handle_app_route_op(
        &self,
        from_node_id: &str,
        assertion: tentaflow_protocol::mesh::SessionAssertion,
        payload_cbor: Vec<u8>,
    ) -> CommandResponse {
        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("app route mesh context is not initialized");
        };
        let (payload_cbor, error) = crate::dispatch::app_route::execute_remote_side(
            from_node_id,
            &assertion,
            &payload_cbor,
            &ctx.iroh,
        )
        .await;
        if let Some(error) = &error {
            warn!(
                from = %from_node_id,
                user = %assertion.sub,
                code = ?error.code,
                "app route: forwarded request refused"
            );
        }
        CommandResponse::ok(MeshCommandResponsePayload::AppRouteOpResult {
            payload_cbor,
            error,
        })
    }

    /// Answer another node's permission-freshness probe (§12.1) from THIS
    /// node's live database — the whole value of the probe is that it sees a
    /// revocation made a moment ago, so nothing here may be cached.
    async fn handle_code_studio_permission_probe(
        &self,
        user_id: String,
        org_id: String,
        workspace_id: String,
        capability: String,
    ) -> CommandResponse {
        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("code studio mesh context is not initialized");
        };
        let answered = tokio::task::spawn_blocking(move || {
            crate::code_studio::remote_proxy::answer_permission_probe(
                &ctx.db,
                &user_id,
                &org_id,
                &workspace_id,
                &capability,
            )
        })
        .await;
        match answered {
            Ok(payload) => CommandResponse::ok(payload),
            Err(e) => CommandResponse::fail(format!("permission probe task failed: {e}")),
        }
    }

    /// Serve one consumer read of a Code Studio stream (§12.2).
    ///
    /// The assertion decides WHO is reading — trust in the peer node is not an
    /// answer to that question (§12.1) — and the hub then only serves the
    /// stream opened for exactly that person on exactly that node. Everything
    /// else, including another user of the same trusted node naming a session
    /// id, gets the answer a stream that does not exist produces.
    async fn handle_code_studio_stream_pull(
        &self,
        from_node_id: &str,
        assertion: tentaflow_protocol::mesh::SessionAssertion,
        request_cbor: Vec<u8>,
    ) -> CommandResponse {
        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("code studio mesh context is not initialized");
        };
        match crate::code_studio::remote_proxy::pull_owner_stream(
            from_node_id,
            &assertion,
            &request_cbor,
            &ctx.iroh,
        )
        .await
        {
            Ok(result) => CommandResponse::ok(MeshCommandResponsePayload::CodeStudioStreamResult {
                frames: result.frames,
                close: result.close,
                highest_seq: result.highest_seq,
            }),
            Err(error) => {
                warn!(
                    from = %from_node_id,
                    sub = %assertion.sub,
                    code = ?error.code,
                    "code studio: stream read refused"
                );
                CommandResponse::fail(error.message)
            }
        }
    }

    /// Open a Code Studio stream for the actor an assertion names (§12.2). The
    /// owner node authorizes from scratch and only then starts producing, so a
    /// stream never exists before somebody was allowed to read it.
    async fn handle_code_studio_stream_open(
        &self,
        from_node_id: &str,
        assertion: tentaflow_protocol::mesh::SessionAssertion,
        request_cbor: Vec<u8>,
    ) -> CommandResponse {
        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("code studio mesh context is not initialized");
        };
        let opened = crate::code_studio::remote_proxy::open_owner_stream(
            from_node_id,
            &assertion,
            &request_cbor,
            &ctx.iroh,
        )
        .await;
        let (highest_seq, error) = match opened {
            Ok(highest_seq) => (highest_seq, None),
            Err(error) => {
                warn!(
                    from = %from_node_id,
                    sub = %assertion.sub,
                    workspace = %assertion.workspace,
                    code = ?error.code,
                    "code studio: stream open refused"
                );
                (0, Some(error))
            }
        };
        CommandResponse::ok(MeshCommandResponsePayload::CodeStudioStreamOpenResult {
            highest_seq,
            error,
        })
    }

    /// Owner side of a forwarded vector op: run it against THIS node's local
    /// Milvus and return the encoded `VectorOpResponse`. The handler module
    /// resolves the local service by id and encodes any failure as a
    /// `VectorOpResponse::Err` payload (never a transport-level error).
    async fn handle_vector_op(&self, request_cbor: Vec<u8>) -> CommandResponse {
        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("vector op remote context is not initialized");
        };
        let result = tokio::task::spawn_blocking(move || {
            crate::services::vector::remote::handle_vector_op_cbor(&ctx.db, &request_cbor)
        })
        .await;
        match result {
            Ok(result_cbor) => {
                CommandResponse::ok(MeshCommandResponsePayload::VectorOpResult { result_cbor })
            }
            Err(e) => CommandResponse::fail(format!("vector op task failed: {}", e)),
        }
    }

    /// Receiver side of a forwarded robot command. Re-checks everything (trust is
    /// already enforced by `execute`): timing, idempotency, robot-addon
    /// resolution, the actor's permission in the request org, then sanitizes the
    /// action and dispatches it to the local robot addon. A pre-execution refusal
    /// (expired / future-dated / unknown robot / permission denied / duplicate) is
    /// returned as a SUCCESSFUL `RobotControlResult` carrying a `rejected`
    /// response — only a missing context / undecodable request is a transport
    /// fail. Move payloads are never logged (no raw movement values in audit).
    async fn handle_robot_control(
        &self,
        from_node_id: &str,
        request_cbor: Vec<u8>,
    ) -> CommandResponse {
        use crate::mesh::robot_control::{
            plan_execution, validate_timing, IdemKey, RobotControlRequest, RobotControlResponse,
        };

        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("robot control context not initialized");
        };

        let req: RobotControlRequest = match ciborium::de::from_reader(&request_cbor[..]) {
            Ok(r) => r,
            Err(e) => return CommandResponse::fail(format!("invalid robot control request: {e}")),
        };

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let respond = |resp: RobotControlResponse| -> CommandResponse {
            let mut buf = Vec::new();
            match ciborium::ser::into_writer(&resp, &mut buf) {
                Ok(()) => CommandResponse::ok(MeshCommandResponsePayload::RobotControlResult {
                    result_cbor: buf,
                }),
                Err(e) => CommandResponse::fail(format!("encode robot control response: {e}")),
            }
        };

        // Timing (expiry / clock-skew / move-duration). A refusal is a normal
        // response, not a transport fail.
        if let Err(reason) = validate_timing(&req, now_ms) {
            warn!(
                robot = %req.robot_id, actor = %req.actor_user_id, from = %from_node_id,
                action = %req.action.audit_label(), reason = ?reason,
                "robot control rejected: timing"
            );
            return respond(RobotControlResponse::rejected(reason));
        }

        let idem_key = IdemKey::from_request(from_node_id, &req);

        // Serialize the check→execute→record critical section per robot so two
        // concurrent identical non-estop commands cannot both miss the cache and
        // actuate twice. E-stop-class actions BYPASS the lock entirely: a stop
        // must execute immediately and never wait behind a queued Move.
        let _exec_guard = if req.action.is_estop_class() {
            None
        } else {
            let lock = self
                .robot_exec_locks
                .entry(req.robot_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone();
            Some(lock.lock_owned().await)
        };

        // Idempotency: a duplicate (non-estop) returns the cached response with no
        // re-execution. Checked INSIDE the held per-robot lock so a concurrent
        // duplicate observes the recorded response instead of re-executing. The
        // std mutex is locked/unlocked here only (never held across an await).
        if let Ok(cache) = self.robot_idem.lock() {
            if let Some(prior) = cache.get(&idem_key, &req.action, now_ms) {
                info!(
                    robot = %req.robot_id, actor = %req.actor_user_id, from = %from_node_id,
                    action = %req.action.audit_label(),
                    "robot control duplicate suppressed"
                );
                return respond(prior);
            }
        }

        // Re-validate timing with a FRESH clock now that we may have waited behind
        // another command for the per-robot lock: a fresh (cache-miss) command that
        // validated just before its deadline must NOT execute after expiry. A
        // duplicate already returned the cached response above, so this only gates
        // never-executed commands. (E-stop bypassed the lock and skips expiry.)
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(now_ms);
        if let Err(reason) = validate_timing(&req, now_ms) {
            warn!(
                robot = %req.robot_id, actor = %req.actor_user_id, from = %from_node_id,
                action = %req.action.audit_label(), reason = ?reason,
                "robot control rejected: timing (post-lock)"
            );
            return respond(RobotControlResponse::rejected(reason));
        }

        // Resolve the local robot addon (deny-by-default) + read its safety cap.
        let resolved = resolve_robot_addon(&ctx.db, &req.robot_id);

        // Authorize the actor on THIS node in the request org (never trust the
        // caller's gate). Missing membership / permission both deny.
        let authorized = crate::services::rbac::permissions::PermissionMatrix::global()
            .has_permission(
                &ctx.db,
                &req.actor_user_id,
                &req.org_id,
                req.action.required_permission(),
            )
            .unwrap_or(false);

        let plan = match plan_execution(&req, resolved.as_ref(), authorized) {
            Ok(plan) => plan,
            Err(reason) => {
                warn!(
                    robot = %req.robot_id, actor = %req.actor_user_id, from = %from_node_id,
                    action = %req.action.audit_label(), reason = ?reason,
                    "robot control rejected"
                );
                return respond(RobotControlResponse::rejected(reason));
            }
        };

        // Dispatch the sanitized call into the addon through the ONE shared
        // local-execute helper (sender(local) reuses the same code). call_tool /
        // invoke_block are synchronous wasmtime calls → run on a blocking thread
        // (like web_research).
        let addon_manager = ctx.addon_manager.clone();
        let plan_for_exec = plan.clone();
        let read_only = req.action.is_read_only();
        let exec = tokio::task::spawn_blocking(move || {
            crate::mesh::robot_control::execute_robot_call(
                &addon_manager,
                &plan_for_exec,
                read_only,
            )
        })
        .await;

        let resp = match exec {
            Ok(resp) => resp,
            Err(e) => RobotControlResponse::failed(format!("robot control task failed: {e}")),
        };

        // Record idempotency (e-stop skipped inside record) + opportunistic evict.
        if let Ok(mut cache) = self.robot_idem.lock() {
            cache.record(idem_key, &req.action, resp.clone(), now_ms);
            cache.evict_expired(now_ms);
        }

        if resp.ok {
            info!(
                robot = %req.robot_id, actor = %req.actor_user_id, from = %from_node_id,
                action = %req.action.audit_label(), addon = %plan.addon_id,
                "robot control accepted"
            );
        } else {
            warn!(
                robot = %req.robot_id, actor = %req.actor_user_id, from = %from_node_id,
                action = %req.action.audit_label(), addon = %plan.addon_id,
                "robot control execution failed"
            );
        }

        respond(resp)
    }

    async fn handle_web_research(&self, request_json: String) -> CommandResponse {
        let Some(ctx) = self.service_action_ctx().await else {
            return CommandResponse::fail("web research remote context is not initialized");
        };
        let request =
            match serde_json::from_str::<crate::web_research::WebResearchRequest>(&request_json) {
                Ok(request) => request,
                Err(e) => {
                    return CommandResponse::fail(format!("invalid web research request: {}", e))
                }
            };
        let result = tokio::task::spawn_blocking(move || {
            crate::web_research::execute_with_local_services(request, &ctx.db)
        })
        .await;
        let response = match result {
            Ok(Ok(response)) => response,
            Ok(Err(e)) => return CommandResponse::fail(e.to_string()),
            Err(e) => return CommandResponse::fail(format!("web research task failed: {}", e)),
        };
        match serde_json::to_string(&response) {
            Ok(response_json) => {
                CommandResponse::ok(MeshCommandResponsePayload::WebResearchResult { response_json })
            }
            Err(e) => CommandResponse::fail(format!("serialize web research response: {}", e)),
        }
    }

    // ----- Cross-node service action handlers (krok N3b) -----

    async fn handle_service_delete_remote(&self, service_id: i64) -> CommandResponse {
        let actions = match self.service_action_ctx().await {
            Some(c) => c,
            None => return CommandResponse::fail("service action context not configured"),
        };
        let _account = match crate::services::coding_agent::lock_account(service_id).await {
            Ok(guard) => guard,
            Err(error) => return CommandResponse::fail(error),
        };
        if let Err(error) = crate::services::account_move::ensure_service_mutation_allowed(&actions.db, service_id, true) {
            return CommandResponse::fail(error.to_string());
        }

        let svc = {
            let conn = match actions.db.read() {
                Ok(c) => c,
                Err(_) => return CommandResponse::fail("db pool poisoned"),
            };
            match crate::services_repo::services::get(&conn, service_id) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    return CommandResponse::fail(format!("service id={} not found", service_id))
                }
                Err(e) => return CommandResponse::fail(e.to_string()),
            }
        };
        // Czlonek AKTYWNEGO klastra TP: usuniecie workera z listy serwisow
        // zabija rank calego distributed-deploymentu serwujacego na innym nodzie.
        // Legalna sciezka = stop deploymentu klastra (teardown kasuje wiersze sam).
        // Osierocony wiersz (deployment juz nie istnieje/nie zyje) przechodzi —
        // user musi moc posprzatac wraki z listy.
        if crate::services::deploy::distributed::service_is_distributed_member(&svc.config_json)
            && crate::services::deploy::distributed::distributed_member_deployment_active(
                &actions.db,
                &svc.config_json,
            )
            .await
        {
            return CommandResponse::fail(
                "serwis jest czlonkiem AKTYWNEGO deploymentu klastra — zatrzymaj deployment klastra zamiast kasowac pojedynczy wiersz",
            );
        }
        // Best-effort runtime stop, then drop the row regardless.
        if let Err(error) = crate::services::deploy::stop(&svc, actions.port_allocator.clone()).await {
            if svc.deploy_method == crate::services_repo::services::DeployMethod::NativeManagedCli {
                return CommandResponse::fail(error.to_string());
            }
        }
        // Scoped lock: drop the MutexGuard before awaiting again.
        {
            let conn = match actions.db.write() {
                Ok(c) => c,
                Err(_) => return CommandResponse::fail("db pool poisoned"),
            };
            if let Err(e) = crate::services_repo::services::delete(&conn, service_id) {
                return CommandResponse::fail(e.to_string());
            }
        }
        push_service_change_after_action(&actions, &self.local_node_id, service_id, true).await;
        CommandResponse::ok(MeshCommandResponsePayload::ServiceActionResult)
    }

    async fn handle_service_pin_remote(&self, service_id: i64, pinned: bool) -> CommandResponse {
        let actions = match self.service_action_ctx().await {
            Some(c) => c,
            None => return CommandResponse::fail("service action context not configured"),
        };
        {
            let conn = match actions.db.write() {
                Ok(c) => c,
                Err(_) => return CommandResponse::fail("db pool poisoned"),
            };
            if let Err(e) = crate::services_repo::services::set_pinned(&conn, service_id, pinned) {
                return CommandResponse::fail(e.to_string());
            }
        }
        push_service_change_after_action(&actions, &self.local_node_id, service_id, false).await;
        CommandResponse::ok(MeshCommandResponsePayload::ServiceActionResult)
    }

    /// Cross-node service edit. Receiver merguje pola opcjonalne do
    /// `services.config_json`, opcjonalnie restartuje serwis (tak samo jak
    /// lokalny `service_update` handler). Zwraca `ServiceActionResult`
    /// (success/error tekst); pełen `ServiceUpdateResponse` z restarted
    /// flag nie idzie przez mesh — caller widzi ack i `push_service_updated`
    /// event przekazuje stan przez normalny snapshot push.
    #[allow(clippy::too_many_arguments)]
    async fn handle_service_update_remote(
        &self,
        service_id: i64,
        model_repo: Option<String>,
        model_preset_id: Option<String>,
        gpu_memory_utilization: Option<f32>,
        max_model_len: Option<u32>,
        max_num_seqs: Option<u32>,
        max_num_batched_tokens: Option<u32>,
        kv_cache_dtype: Option<String>,
        chunked_prefill: Option<bool>,
        vllm_args_override: Option<String>,
        pinned: Option<bool>,
        paused: Option<bool>,
        restart_after_save: bool,
    ) -> CommandResponse {
        let actions = match self.service_action_ctx().await {
            Some(c) => c,
            None => return CommandResponse::fail("service action context not configured"),
        };
        let _account = match crate::services::coding_agent::lock_account(service_id).await {
            Ok(guard) => guard,
            Err(error) => return CommandResponse::fail(error),
        };
        if let Err(error) = crate::services::account_move::ensure_service_mutation_allowed(&actions.db, service_id, false) {
            return CommandResponse::fail(error.to_string());
        }


        let svc = {
            let conn = match actions.db.read() {
                Ok(c) => c,
                Err(_) => return CommandResponse::fail("db pool poisoned"),
            };
            match crate::services_repo::services::get(&conn, service_id) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    return CommandResponse::fail(format!("service id={} not found", service_id));
                }
                Err(e) => return CommandResponse::fail(e.to_string()),
            }
        };

        // Merge config_json (sama logika co handler local).
        let mut cfg: serde_json::Value =
            serde_json::from_str(&svc.config_json).unwrap_or_else(|_| serde_json::json!({}));
        let Some(cfg_obj) = cfg.as_object_mut() else {
            return CommandResponse::fail("service config_json is not an object");
        };
        if let Some(repo) = model_repo {
            cfg_obj.insert("model_repo".into(), serde_json::Value::String(repo));
            cfg_obj.insert("model_preset_id".into(), serde_json::Value::Null);
        }
        if let Some(preset_id) = model_preset_id {
            cfg_obj.insert(
                "model_preset_id".into(),
                serde_json::Value::String(preset_id),
            );
            cfg_obj.insert("model_repo".into(), serde_json::Value::Null);
        }
        // Same merge as the local handler: scalar keys + flags rewritten inside
        // `vllm_args`, otherwise the edit never reaches a vLLM/sglang engine.
        crate::deploy::launch_dialect::apply_service_tuning(
            &svc.engine_id,
            cfg_obj,
            &crate::deploy::launch_dialect::ServiceTuningPatch {
                gpu_memory_utilization,
                max_model_len,
                max_num_seqs,
                max_num_batched_tokens,
                kv_cache_dtype,
                chunked_prefill,
                vllm_args_override,
            },
        );
        let new_config_json = match serde_json::to_string(&cfg) {
            Ok(s) => s,
            Err(e) => return CommandResponse::fail(format!("serialize config: {}", e)),
        };

        {
            let conn = match actions.db.write() {
                Ok(c) => c,
                Err(_) => return CommandResponse::fail("db pool poisoned"),
            };
            if let Err(e) = crate::services_repo::services::update_config_json(
                &conn,
                service_id,
                &new_config_json,
            ) {
                return CommandResponse::fail(e.to_string());
            }
            if let Some(p) = pinned {
                if let Err(e) = crate::services_repo::services::set_pinned(&conn, service_id, p) {
                    return CommandResponse::fail(e.to_string());
                }
            }
            if let Some(p) = paused {
                if let Err(e) = crate::services_repo::services::set_paused(&conn, service_id, p) {
                    return CommandResponse::fail(e.to_string());
                }
            }
        }

        // Optional restart — stop running runtime + spawn detached respawn
        // (mirror lokalnego handler'a żeby UX był identyczny).
        let was_running = matches!(
            svc.status,
            crate::services_repo::services::ServiceStatus::Running
                | crate::services_repo::services::ServiceStatus::Degraded
                | crate::services_repo::services::ServiceStatus::Starting
        );
        if restart_after_save && was_running {
            let ports = actions.port_allocator.clone();
            if let Err(e) = crate::services::deploy::stop(&svc, ports.clone()).await {
                if svc.deploy_method == crate::services_repo::services::DeployMethod::NativeManagedCli {
                    return CommandResponse::fail(e.to_string());
                }
                tracing::warn!(service_id, "service_update_remote: stop failed: {}", e);
            }
            {
                let conn = match actions.db.write() {
                    Ok(c) => c,
                    Err(_) => return CommandResponse::fail("db pool poisoned"),
                };
                let _ = crate::services_repo::services::update_status(
                    &conn,
                    service_id,
                    crate::services_repo::services::ServiceStatus::Starting,
                );
            }
            let db = actions.db.clone();
            let settings_cipher = self.security.settings_cipher().clone();
            let engine_id = svc.engine_id.clone();
            let deploy_method = svc.deploy_method;
            let cfg_json_for_task = new_config_json.clone();
            let preserved_port = svc.runtime_port;
            tokio::spawn(async move {
                let _account = _account;
                match crate::services::deploy::respawn(
                    &engine_id,
                    deploy_method,
                    &cfg_json_for_task,
                    ports,
                    &db,
                    &settings_cipher,
                    preserved_port,
                )
                .await
                {
                    Ok(handle) => {
                        if let Ok(conn) = db.write() {
                            let _ = crate::services_repo::services::update_runtime(
                                &conn,
                                service_id,
                                handle.pid,
                                handle.port,
                                handle.sidecar_port,
                                handle.endpoint_url.as_deref(),
                            );
                            let _ = crate::services_repo::services::update_status(
                                &conn,
                                service_id,
                                crate::services_repo::services::ServiceStatus::Running,
                            );
                        }
                    }
                    Err(e) => {
                        let msg = format!("respawn after update_remote: {}", e);
                        if let Ok(conn) = db.write() {
                            let _ = crate::services_repo::services::update_status(
                                &conn,
                                service_id,
                                crate::services_repo::services::ServiceStatus::Failed,
                            );
                            let _ = crate::services_repo::services::update_health(
                                &conn,
                                service_id,
                                false,
                                Some(&msg),
                            );
                        }
                    }
                }
            });
        }

        push_service_change_after_action(&actions, &self.local_node_id, service_id, false).await;
        CommandResponse::ok(MeshCommandResponsePayload::ServiceActionResult)
    }

    async fn handle_service_pause_remote(&self, service_id: i64, paused: bool) -> CommandResponse {
        let actions = match self.service_action_ctx().await {
            Some(c) => c,
            None => return CommandResponse::fail("service action context not configured"),
        };
        let _account = match crate::services::coding_agent::lock_account(service_id).await {
            Ok(guard) => guard,
            Err(error) => return CommandResponse::fail(error),
        };
        if let Err(error) = crate::services::account_move::ensure_service_mutation_allowed(&actions.db, service_id, false) {
            return CommandResponse::fail(error.to_string());
        }


        // When pausing, mirror the local handler: actively stop the runtime
        // and clear runtime metadata so health checks don't keep flapping.
        if paused {
            let svc = {
                let conn = match actions.db.read() {
                    Ok(c) => c,
                    Err(_) => return CommandResponse::fail("db pool poisoned"),
                };
                match crate::services_repo::services::get(&conn, service_id) {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        return CommandResponse::fail(format!(
                            "service id={} not found",
                            service_id
                        ))
                    }
                    Err(e) => return CommandResponse::fail(e.to_string()),
                }
            };
            if matches!(
                svc.status,
                crate::services_repo::services::ServiceStatus::Running
                    | crate::services_repo::services::ServiceStatus::Degraded
                    | crate::services_repo::services::ServiceStatus::Starting
            ) {
                if let Err(e) =
                    crate::services::deploy::stop(&svc, actions.port_allocator.clone()).await
                {
                    return CommandResponse::fail(e.to_string());
                }
                let conn = match actions.db.write() {
                    Ok(c) => c,
                    Err(_) => return CommandResponse::fail("db pool poisoned"),
                };
                if let Err(e) = crate::services_repo::services::update_status(
                    &conn,
                    service_id,
                    crate::services_repo::services::ServiceStatus::Stopped,
                ) {
                    return CommandResponse::fail(e.to_string());
                }
                if let Err(e) = crate::services_repo::services::update_runtime(
                    &conn, service_id, None, None, None, None,
                ) {
                    return CommandResponse::fail(e.to_string());
                }
            }
        }

        {
            let conn = match actions.db.write() {
                Ok(c) => c,
                Err(_) => return CommandResponse::fail("db pool poisoned"),
            };
            if let Err(e) = crate::services_repo::services::set_paused(&conn, service_id, paused) {
                return CommandResponse::fail(e.to_string());
            }
        }
        push_service_change_after_action(&actions, &self.local_node_id, service_id, false).await;
        CommandResponse::ok(MeshCommandResponsePayload::ServiceActionResult)
    }

    async fn handle_service_start_remote(&self, service_id: i64) -> CommandResponse {
        let actions = match self.service_action_ctx().await {
            Some(c) => c,
            None => return CommandResponse::fail("service action context not configured"),
        };
        let _account = match crate::services::coding_agent::lock_account(service_id).await {
            Ok(guard) => guard,
            Err(error) => return CommandResponse::fail(error),
        };
        if let Err(error) = crate::services::account_move::ensure_service_mutation_allowed(&actions.db, service_id, false) {
            return CommandResponse::fail(error.to_string());
        }

        let svc = {
            let conn = match actions.db.read() {
                Ok(c) => c,
                Err(_) => return CommandResponse::fail("db pool poisoned"),
            };
            match crate::services_repo::services::get(&conn, service_id) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    return CommandResponse::fail(format!("service id={} not found", service_id))
                }
                Err(e) => return CommandResponse::fail(e.to_string()),
            }
        };

        // Idempotent for already-running services.
        if matches!(
            svc.status,
            crate::services_repo::services::ServiceStatus::Running
                | crate::services_repo::services::ServiceStatus::Degraded
        ) && !svc.paused
        {
            return CommandResponse::ok(MeshCommandResponsePayload::ServiceActionResult);
        }

        // Clear pause + flip to Starting before respawn.
        {
            let conn = match actions.db.write() {
                Ok(c) => c,
                Err(_) => return CommandResponse::fail("db pool poisoned"),
            };
            if svc.paused {
                if let Err(e) = crate::services_repo::services::set_paused(&conn, service_id, false)
                {
                    return CommandResponse::fail(e.to_string());
                }
            }
            if let Err(e) = crate::services_repo::services::update_status(
                &conn,
                service_id,
                crate::services_repo::services::ServiceStatus::Starting,
            ) {
                return CommandResponse::fail(e.to_string());
            }
        }

        let respawn = crate::services::deploy::respawn(
            &svc.engine_id,
            svc.deploy_method,
            &svc.config_json,
            actions.port_allocator.clone(),
            &actions.db,
            self.security.settings_cipher(),
            svc.runtime_port,
        )
        .await;

        let result = match respawn {
            Ok(handle) => {
                let conn = match actions.db.write() {
                    Ok(c) => c,
                    Err(_) => return CommandResponse::fail("db pool poisoned"),
                };
                if let Err(e) = crate::services_repo::services::update_runtime(
                    &conn,
                    service_id,
                    handle.pid,
                    handle.port,
                    handle.sidecar_port,
                    handle.endpoint_url.as_deref(),
                ) {
                    return CommandResponse::fail(e.to_string());
                }
                if let Err(e) = crate::services_repo::services::update_status(
                    &conn,
                    service_id,
                    crate::services_repo::services::ServiceStatus::Running,
                ) {
                    return CommandResponse::fail(e.to_string());
                }
                CommandResponse::ok(MeshCommandResponsePayload::ServiceActionResult)
            }
            Err(e) => {
                let msg = e.to_string();
                if let Ok(conn) = actions.db.write() {
                    let _ = crate::services_repo::services::update_status(
                        &conn,
                        service_id,
                        crate::services_repo::services::ServiceStatus::Failed,
                    );
                    let _ = crate::services_repo::services::update_health(
                        &conn,
                        service_id,
                        false,
                        Some(&msg),
                    );
                }
                CommandResponse::fail(msg)
            }
        };

        push_service_change_after_action(&actions, &self.local_node_id, service_id, false).await;
        result
    }

    async fn handle_service_deploy_remote(
        &self,
        requester_node_id: &str,
        engine_id: &str,
        deploy_method: &str,
        config_json: &str,
    ) -> CommandResponse {
        let actions = match self.service_action_ctx().await {
            Some(c) => c,
            None => return CommandResponse::fail("service action context not configured"),
        };

        let manifest = match crate::services::manifest::registry().by_id(engine_id) {
            Some(m) => m.clone(),
            None => {
                return CommandResponse::fail(format!(
                    "engine '{}' not found in manifest",
                    engine_id
                ))
            }
        };

        let resolved = match resolve_deploy_method(&manifest, deploy_method) {
            Ok(m) => m,
            Err(e) => return CommandResponse::fail(e),
        };

        let user_config: serde_json::Value = if config_json.is_empty() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            match serde_json::from_str(config_json) {
                Ok(v) => v,
                Err(e) => return CommandResponse::fail(format!("invalid config_json: {}", e)),
            }
        };
        // Subscription login: the OAuth flow ran on THIS node (forwarded
        // OauthStart/Poll), so swap the `oauth_flow_id` for the tokens captured
        // in this node's local flow store.
        let user_config = {
            let mut cfg = user_config;
            if let Some(obj) = cfg.as_object_mut() {
                if let Some(flow_id) = obj
                    .get("oauth_flow_id")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                {
                    if let Some(blob) = crate::services::backend::codex_oauth::take_tokens(&flow_id)
                    {
                        obj.insert("api_key".to_string(), serde_json::Value::String(blob));
                    }
                    obj.remove("oauth_flow_id");
                }
            }
            cfg
        };
        // Cloud external provider keys arrive plaintext over the encrypted mesh;
        // re-encrypt with THIS node's settings cipher so the key is node-local
        // and never persisted in the clear.
        let user_config = crate::services::deploy::encrypt_api_key_in_config(
            &user_config,
            self.security.settings_cipher(),
        );

        let job = match crate::services::deploy::create_deploy_job(
            resolved,
            &manifest,
            &user_config,
            &actions.db,
            &self.local_node_id,
            None,
            None,
        ) {
            Ok(job) => job,
            Err(e) => return CommandResponse::fail(e.to_string()),
        };

        if let Ok(Some(info)) = crate::services::snapshot_builder::build_one(
            &actions.db,
            job.service_id,
            &self.local_node_id,
        ) {
            let payload = tentaflow_protocol::mesh::MeshServicesUpdatePayload {
                from_node_id: self.local_node_id.clone(),
                change: tentaflow_protocol::ServiceChange::Added(info),
            };
            if let Ok(bytes) = crate::mesh::cbor::encode(&payload) {
                let _ = actions
                    .iroh
                    .broadcast_ufp2_to_trusted(
                        tentaflow_protocol::mesh::MESH_MSG_SERVICES_UPDATE,
                        &bytes,
                        None,
                    )
                    .await;
            }
        }

        let slug = job.deploy_id.clone();
        let log_sender = crate::deploy::log_bus::sender_for(&slug);
        let db_clone = actions.db.clone();
        // Token HF rozwiazujemy z secure setting TEGO noda (odbiorcy) wewnatrz
        // deploy() — nigdy nie jest forwardowany przez mesh.
        let settings_cipher_task = self.security.settings_cipher().clone();
        let port_alloc = actions.port_allocator.clone();
        let job_task = job.clone();
        let manifest_task = manifest.clone();
        let user_config_task = user_config.clone();
        let log_sender_task = log_sender.clone();
        let slug_task = slug.clone();
        let local_node_id_task = self.local_node_id.clone();
        let iroh_task = actions.iroh.clone();

        {
            let mut progress_rx = log_sender.subscribe();
            let iroh_progress = actions.iroh.clone();
            let local_node_id_progress = self.local_node_id.clone();
            let requester_node_id_progress = requester_node_id.to_string();
            let db_progress = actions.db.clone();
            let service_id_progress = job.service_id;
            tokio::spawn(async move {
                loop {
                    match progress_rx.recv().await {
                        Ok(crate::deploy::log_bus::BusMessage::Line(line)) => {
                            let should_publish_service =
                                line.kind == "phase" || line.kind == "progress";
                            if should_publish_service {
                                if let Ok(Some(info)) = crate::services::snapshot_builder::build_one(
                                    &db_progress,
                                    service_id_progress,
                                    &local_node_id_progress,
                                ) {
                                    let payload =
                                        tentaflow_protocol::mesh::MeshServicesUpdatePayload {
                                            from_node_id: local_node_id_progress.clone(),
                                            change: tentaflow_protocol::ServiceChange::Updated(
                                                info,
                                            ),
                                        };
                                    if let Ok(bytes) = crate::mesh::cbor::encode(&payload) {
                                        let _ = iroh_progress
                                            .broadcast_ufp2_to_trusted(
                                                tentaflow_protocol::mesh::MESH_MSG_SERVICES_UPDATE,
                                                &bytes,
                                                None,
                                            )
                                            .await;
                                    }
                                }
                            }
                            let message = if line.line.is_empty() {
                                line.phase.clone()
                            } else {
                                line.line.clone()
                            };
                            let payload =
                                tentaflow_protocol::mesh::MeshMessage::MeshDeployProgress {
                                    command_id: line.deploy_id,
                                    from_node_id: local_node_id_progress.clone(),
                                    phase: line.kind,
                                    message,
                                    percent: line.progress_pct.min(100) as u8,
                                    is_done: false,
                                };
                            if let Ok(bytes) = crate::mesh::cbor::encode(&payload) {
                                let _ = iroh_progress
                                    .send_ufp2_to_peer(
                                        &requester_node_id_progress,
                                        tentaflow_protocol::mesh::MESH_MSG_DEPLOY_PROGRESS,
                                        &bytes,
                                    )
                                    .await;
                            }
                        }
                        Ok(crate::deploy::log_bus::BusMessage::End {
                            deploy_id,
                            final_status,
                            error_message,
                            ..
                        }) => {
                            let payload =
                                tentaflow_protocol::mesh::MeshMessage::MeshDeployProgress {
                                    command_id: deploy_id,
                                    from_node_id: local_node_id_progress.clone(),
                                    phase: final_status,
                                    message: error_message,
                                    percent: 100,
                                    is_done: true,
                                };
                            if let Ok(bytes) = crate::mesh::cbor::encode(&payload) {
                                let _ = iroh_progress
                                    .send_ufp2_to_peer(
                                        &requester_node_id_progress,
                                        tentaflow_protocol::mesh::MESH_MSG_DEPLOY_PROGRESS,
                                        &bytes,
                                    )
                                    .await;
                            }
                            return;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    }
                }
            });
        }

        tokio::spawn(async move {
            let start_ms = crate::deploy::log_bus::now_ms();
            let result = crate::services::deploy::deploy(
                job_task.clone(),
                resolved,
                &manifest_task,
                &user_config_task,
                &port_alloc,
                &db_clone,
                &settings_cipher_task,
                Some(log_sender_task.clone()),
            )
            .await;
            match result {
                Ok(outcome) => {
                    let _ = log_sender_task.send(crate::deploy::log_bus::BusMessage::End {
                        deploy_id: slug_task.clone(),
                        final_status: "success".to_string(),
                        image_tag: String::new(),
                        container_name: format!("service-id-{}", outcome.endpoint.handle.id),
                        error_message: String::new(),
                        duration_ms: crate::deploy::log_bus::now_ms() - start_ms,
                    });
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    crate::deploy::log_bus::close(&slug_task);
                    let service_id = outcome.endpoint.handle.id;
                    if let Ok(Some(info)) = crate::services::snapshot_builder::build_one(
                        &db_clone,
                        service_id,
                        &local_node_id_task,
                    ) {
                        let payload = tentaflow_protocol::mesh::MeshServicesUpdatePayload {
                            from_node_id: local_node_id_task.clone(),
                            change: tentaflow_protocol::ServiceChange::Updated(info),
                        };
                        if let Ok(bytes) = crate::mesh::cbor::encode(&payload) {
                            let _ = iroh_task
                                .broadcast_ufp2_to_trusted(
                                    tentaflow_protocol::mesh::MESH_MSG_SERVICES_UPDATE,
                                    &bytes,
                                    None,
                                )
                                .await;
                        }
                    }
                }
                Err(err) => {
                    let _ = log_sender_task.send(crate::deploy::log_bus::BusMessage::End {
                        deploy_id: slug_task.clone(),
                        final_status: "failed".to_string(),
                        image_tag: String::new(),
                        container_name: String::new(),
                        error_message: err.to_string(),
                        duration_ms: crate::deploy::log_bus::now_ms() - start_ms,
                    });
                    if let Ok(Some(info)) = crate::services::snapshot_builder::build_one(
                        &db_clone,
                        job_task.service_id,
                        &local_node_id_task,
                    ) {
                        let payload = tentaflow_protocol::mesh::MeshServicesUpdatePayload {
                            from_node_id: local_node_id_task.clone(),
                            change: tentaflow_protocol::ServiceChange::Updated(info),
                        };
                        if let Ok(bytes) = crate::mesh::cbor::encode(&payload) {
                            let _ = iroh_task
                                .broadcast_ufp2_to_trusted(
                                    tentaflow_protocol::mesh::MESH_MSG_SERVICES_UPDATE,
                                    &bytes,
                                    None,
                                )
                                .await;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    crate::deploy::log_bus::close(&slug_task);
                }
            }
        });

        CommandResponse::ok(MeshCommandResponsePayload::ServiceDeployResult {
            deploy_id: slug,
            engine_id: engine_id.to_string(),
            deploy_method: deploy_method.to_string(),
        })
    }

    /// Distributed (multi-node TP) deploy JEDNEGO slice'a modelu NA TYM nodzie.
    /// Buduje `user_config` z `_distributed` + komenda `ray ... && vllm serve` +
    /// NCCL env, po czym REUZYWA dokladnie ten sam potok co `ServiceDeployRemote`
    /// (`handle_service_deploy_remote`: create_deploy_job + deploy() + log stream +
    /// broadcast rejestru). `DockerDeploy` rozpoznaje `_distributed` i przelacza na
    /// host-networking + RDMA. Zwraca slug, nazwe kontenera i endpoint (head only).
    async fn handle_service_deploy_distributed(
        &self,
        requester_node_id: &str,
        spec: tentaflow_protocol::mesh::DistributedDeploySpec,
    ) -> CommandResponse {
        // Preflight (P1-4): clean stale Ray containers from a prior attempt and,
        // for the head, verify the serve + GCS ports are free BEFORE launching
        // (host networking offers no port protection). Fail clearly otherwise.
        if let Err(e) = crate::services::deploy::distributed::preflight_member(&spec).await {
            return CommandResponse::fail(e);
        }
        let config_json =
            match crate::services::deploy::distributed::build_member_config_json(&spec) {
                Ok(c) => c,
                Err(e) => return CommandResponse::fail(e),
            };
        let resp = self
            .handle_service_deploy_remote(
                requester_node_id,
                &spec.engine_id,
                "docker",
                &config_json,
            )
            .await;
        if !resp.ok {
            return resp;
        }
        let deploy_id = match &resp.payload {
            MeshCommandResponsePayload::ServiceDeployResult { deploy_id, .. } => deploy_id.clone(),
            _ => String::new(),
        };
        let container_name =
            crate::services::deploy::distributed::container_name(&spec.engine_id, spec.port);
        let endpoint_url = crate::services::deploy::distributed::endpoint_url_for(&spec);
        CommandResponse::ok(MeshCommandResponsePayload::ServiceDeployDistributedResult {
            deploy_id,
            container_name,
            endpoint_url,
        })
    }

    /// Teardown distributed-deploymentu NA TYM nodzie: usuwa kontener(y) po
    /// etykiecie `deployment_cluster_id` ORAZ kasuje wiersze serwisow niosace ten
    /// id (head + workery), broadcastujac usuniecie do reszty mesh. Idempotentne.
    async fn handle_service_stop_distributed(
        &self,
        deployment_cluster_id: &str,
    ) -> CommandResponse {
        let actions = match self.service_action_ctx().await {
            Some(c) => c,
            None => return CommandResponse::fail("service action context not configured"),
        };
        let (removed, errors) = crate::services::deploy::distributed::stop_distributed(
            &actions.db,
            actions.port_allocator.clone(),
            deployment_cluster_id,
        )
        .await;
        for id in removed {
            push_service_change_after_action(&actions, &self.local_node_id, id, true).await;
        }
        // P1-2: incomplete teardown must surface as a failure so the coordinator
        // keeps the deployment record for retry (no silently-orphaned Ray).
        if errors.is_empty() {
            CommandResponse::ok(MeshCommandResponsePayload::Empty)
        } else {
            CommandResponse::fail(format!("teardown niekompletny: {}", errors.join("; ")))
        }
    }

    /// Zapisuje certyfikaty do dozwolonego katalogu
    async fn handle_provision_certs(
        &self,
        cert_pem: &str,
        key_pem: &str,
        target_dir: &str,
    ) -> CommandResponse {
        match self.validate_target_dir(target_dir) {
            Ok(dir) => {
                let cert_path = dir.join("cert.pem");
                let key_path = dir.join("key.pem");

                if let Err(e) = tokio::fs::write(&cert_path, cert_pem).await {
                    return CommandResponse::fail(format!("Blad zapisu cert.pem: {}", e));
                }

                if let Err(e) = tokio::fs::write(&key_path, key_pem).await {
                    return CommandResponse::fail(format!("Blad zapisu key.pem: {}", e));
                }

                info!(dir = %dir.display(), "Certyfikaty zapisane");

                CommandResponse::ok(MeshCommandResponsePayload::Text(format!(
                    "Certyfikaty zapisane w {}",
                    dir.display()
                )))
            }
            Err(msg) => CommandResponse::fail(msg),
        }
    }

    /// Waliduje sciezke docelowa — rozwiazuje symlinki przez canonicalize,
    /// sprawdza Path::starts_with() po komponentach sciezki
    fn validate_target_dir(&self, target_dir: &str) -> Result<PathBuf, String> {
        let expanded = if target_dir.starts_with("~/") {
            match dirs::home_dir() {
                Some(home) => home.join(&target_dir[2..]),
                None => return Err("Nie udalo sie ustalic katalogu domowego".to_string()),
            }
        } else {
            PathBuf::from(target_dir)
        };

        // Znajdz najdluzszy istniejacy prefix sciezki i canonicalize go,
        // potem dolacz reszte — to rozwiazuje symlinki bez wymagania istnienia katalogu
        let canonical = Self::safe_canonicalize(&expanded)?;

        let home_tentaflow = dirs::home_dir().map(|h| h.join(".tentaflow"));
        let data_tentaflow = dirs::data_dir().map(|d| d.join("tentaflow"));

        let mut allowed_dirs: Vec<PathBuf> = Vec::new();
        if let Some(p) = home_tentaflow {
            allowed_dirs.push(p);
        }
        if let Some(p) = data_tentaflow {
            allowed_dirs.push(p);
        }

        // Sprawdzenie po komponentach sciezki (Path::starts_with)
        let is_allowed = allowed_dirs.iter().any(|allowed| {
            let allowed_canonical =
                Self::safe_canonicalize(allowed).unwrap_or_else(|_| allowed.clone());
            canonical.starts_with(&allowed_canonical)
        });

        if !is_allowed {
            return Err(format!(
                "Sciezka '{}' poza dozwolonym katalogiem (~/.tentaflow/ lub data dir)",
                target_dir
            ));
        }

        // Utworz katalog dopiero PO walidacji
        std::fs::create_dir_all(&canonical)
            .map_err(|e| format!("Nie mozna utworzyc katalogu: {}", e))?;

        Ok(canonical)
    }

    /// Rozwiazuje sciezke przez canonicalize istniejacego prefixu + normalizacje reszty
    fn safe_canonicalize(path: &std::path::Path) -> Result<PathBuf, String> {
        // Probuj canonicalize calej sciezki
        if let Ok(c) = std::fs::canonicalize(path) {
            return Ok(c);
        }

        // Znajdz najdluzszy istniejacy prefix
        let mut existing = path.to_path_buf();
        let mut suffix_parts: Vec<std::ffi::OsString> = Vec::new();

        loop {
            if existing.exists() {
                break;
            }
            match existing.file_name() {
                Some(part) => {
                    suffix_parts.push(part.to_os_string());
                    existing.pop();
                }
                None => break,
            }
        }

        let base = std::fs::canonicalize(&existing)
            .map_err(|e| format!("Nie mozna rozwiazac sciezki: {}", e))?;

        let mut result = base;
        for part in suffix_parts.into_iter().rev() {
            result.push(part);
        }

        Ok(result)
    }

    // -------------------------------------------------------------------------
    // Multi-source profiling handlery — wykonywane na nodzie odbierajacym
    // komende mesh. Lokalny dispatch w `mesh_write_handlers.rs::handle_profiling_local`
    // zawiera te sama logike (z dodatkowym audit log + auth) wolana przy local
    // node_id. Tu obslugujemy peer-side, gdzie auth juz przeszlo przez `is_trusted`.
    // -------------------------------------------------------------------------

    fn map_skipped_v2(
        v: Vec<crate::profiling::SkippedCollector>,
    ) -> Vec<tentaflow_protocol::ProfilingSkippedCollector> {
        v.into_iter()
            .map(|s| tentaflow_protocol::ProfilingSkippedCollector {
                id: s.id,
                reason: s.reason,
            })
            .collect()
    }

    fn map_session_entry_v2(
        e: crate::profiling::SessionEntry,
    ) -> tentaflow_protocol::ProfilingSessionEntry {
        let kind = match e.kind {
            crate::profiling::SessionKind::MultiSource => "multi_source".to_string(),
        };
        tentaflow_protocol::ProfilingSessionEntry {
            session_id: e.session_id,
            label: e.label,
            started_at: e.started_at,
            duration_ns: e.duration_ns,
            kind,
            collectors_used: e.collectors_used,
            size_bytes: e.size_bytes,
        }
    }

    async fn handle_profiling_start(
        &self,
        req: tentaflow_protocol::ProfilingStartRequest,
    ) -> CommandResponse {
        use crate::profiling::{ElevationToken, MULTI_SOURCE, PROFILE_PARSERS};
        use std::time::{SystemTime, UNIX_EPOCH};
        let elevation = if req.elevation_password.is_empty() {
            None
        } else {
            Some(std::sync::Arc::new(ElevationToken::new_sudo(
                req.elevation_password.clone(),
            )))
        };
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u128)
            .unwrap_or(0);
        let session_id = format!(
            "{:016x}{:016x}",
            nanos as u64,
            (nanos >> 64) as u64 ^ 0x9e37_79b9_7f4a_7c15
        );
        let orchestrator = std::sync::Arc::clone(&MULTI_SOURCE);
        let parsers = std::sync::Arc::clone(&PROFILE_PARSERS);
        match orchestrator
            .clone()
            .start(
                req.scope,
                self.local_node_id.clone(),
                session_id,
                req.label,
                elevation,
                parsers,
            )
            .await
        {
            Ok(handle) => match orchestrator.active_info().await {
                Some(info) => CommandResponse::ok(MeshCommandResponsePayload::ProfilingStart(
                    tentaflow_protocol::ProfilingStartResponse {
                        session_id: handle.session_id,
                        started_at_unix_ns: info.started_at_unix_ns,
                        collectors_started: info.collectors_running,
                        collectors_skipped: Self::map_skipped_v2(info.collectors_skipped),
                    },
                )),
                None => CommandResponse::fail("orchestrator lost active session".to_string()),
            },
            Err(e) => CommandResponse::fail(format!("profiling start: {}", e)),
        }
    }

    async fn handle_profiling_stop(
        &self,
        req: tentaflow_protocol::ProfilingStopRequest,
    ) -> CommandResponse {
        use crate::profiling::MULTI_SOURCE;
        let orchestrator = std::sync::Arc::clone(&MULTI_SOURCE);
        match orchestrator.clone().stop_by_id(&req.session_id).await {
            Ok(report) => CommandResponse::ok(MeshCommandResponsePayload::ProfilingStop(
                tentaflow_protocol::ProfilingStopResponse {
                    session_id: report.session_id.clone(),
                    report,
                },
            )),
            Err(e) => CommandResponse::fail(format!("profiling stop: {}", e)),
        }
    }

    async fn handle_profiling_sessions(
        &self,
        req: tentaflow_protocol::ProfilingSessionsRequest,
    ) -> CommandResponse {
        use crate::profiling::PROFILE_STORAGE;
        match PROFILE_STORAGE.list_sessions(&self.local_node_id).await {
            Ok(entries) => {
                let entries = entries
                    .into_iter()
                    .map(Self::map_session_entry_v2)
                    .collect();
                CommandResponse::ok(MeshCommandResponsePayload::ProfilingSessions(
                    tentaflow_protocol::ProfilingSessionsResponse {
                        node_id: req.node_id,
                        entries,
                    },
                ))
            }
            Err(e) => CommandResponse::fail(format!("profiling sessions: {}", e)),
        }
    }

    async fn handle_profiling_report(
        &self,
        req: tentaflow_protocol::ProfilingReportRequest,
    ) -> CommandResponse {
        use crate::profiling::PROFILE_STORAGE;
        match PROFILE_STORAGE
            .read_report(&self.local_node_id, &req.session_id)
            .await
        {
            Ok(report) => CommandResponse::ok(MeshCommandResponsePayload::ProfilingReport(
                tentaflow_protocol::ProfilingReportResponse { report },
            )),
            Err(e) => CommandResponse::fail(format!("profiling report: {}", e)),
        }
    }

    async fn handle_profiling_delete(
        &self,
        req: tentaflow_protocol::ProfilingDeleteRequest,
    ) -> CommandResponse {
        use crate::profiling::PROFILE_STORAGE;
        match PROFILE_STORAGE
            .delete_session(&self.local_node_id, &req.session_id)
            .await
        {
            Ok(()) => CommandResponse::ok(MeshCommandResponsePayload::ProfilingDelete(
                tentaflow_protocol::ProfilingDeleteResponse {
                    session_id: req.session_id,
                    deleted: true,
                },
            )),
            Err(e) => CommandResponse::fail(format!("profiling delete: {}", e)),
        }
    }

    async fn handle_profiling_download(
        &self,
        req: tentaflow_protocol::ProfilingDownloadRequest,
    ) -> CommandResponse {
        use crate::profiling::PROFILE_STORAGE;
        use std::io::Write;
        let storage = std::sync::Arc::clone(&PROFILE_STORAGE);
        let node_id = self.local_node_id.clone();
        let sid = req.session_id.clone();
        let bytes_res = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
            let session_dir = storage.root().join(&node_id).join(&sid);
            if !session_dir.exists() {
                return Err(format!("session {sid} not found"));
            }
            let buf: Vec<u8> = Vec::new();
            let encoder = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
            let mut tar = tar::Builder::new(encoder);
            tar.append_dir_all(&sid, &session_dir)
                .map_err(|e| format!("tar: {e}"))?;
            let mut encoder = tar.into_inner().map_err(|e| format!("tar finalize: {e}"))?;
            encoder.flush().map_err(|e| format!("gzip flush: {e}"))?;
            encoder.finish().map_err(|e| format!("gzip finish: {e}"))
        })
        .await;
        match bytes_res {
            Ok(Ok(bytes)) => {
                let filename = format!("profiling-{}.tar.gz", req.session_id);
                CommandResponse::ok(MeshCommandResponsePayload::ProfilingDownload(
                    tentaflow_protocol::ProfilingDownloadResponse {
                        session_id: req.session_id,
                        filename,
                        tarball_bytes: bytes,
                    },
                ))
            }
            Ok(Err(msg)) => CommandResponse::fail(format!("profiling download: {msg}")),
            Err(e) => CommandResponse::fail(format!("profiling download join: {e}")),
        }
    }

    async fn handle_profiling_active_info(
        &self,
        _req: tentaflow_protocol::ProfilingActiveInfoRequest,
    ) -> CommandResponse {
        use crate::profiling::MULTI_SOURCE;
        let info = MULTI_SOURCE.active_info().await.map(|i| {
            tentaflow_protocol::ProfilingActiveSessionInfo {
                session_id: i.session_id,
                node_id: i.node_id,
                label: i.label,
                started_at_unix_ns: i.started_at_unix_ns,
                planned_duration_ns: i.planned_duration_ns,
                elapsed_ns: i.elapsed_ns,
                collectors_running: i.collectors_running,
                collectors_skipped: Self::map_skipped_v2(i.collectors_skipped),
            }
        });
        CommandResponse::ok(MeshCommandResponsePayload::ProfilingActiveInfo(
            tentaflow_protocol::ProfilingActiveInfoResponse { info },
        ))
    }

    // -------------------------------------------------------------------------
    // Docker handlery (bollard) — operacje na lokalnym daemonie Docker
    // wykonywane na zlecenie zaufanego peera. Polaczenie nawiazywane on-demand,
    // tym samym kanalem co `deploy/docker.rs` (unix socket / npipe).
    // -------------------------------------------------------------------------

    #[cfg(feature = "docker")]
    async fn connect_docker() -> Result<bollard::Docker, String> {
        bollard::Docker::connect_with_local_defaults()
            .map_err(|e| format!("Polaczenie z Docker daemon nieudane: {}", e))
    }

    /// Walidacja identyfikatora kontenera — Docker akceptuje hex (12/64 znakow)
    /// albo nazwy `[a-zA-Z0-9][a-zA-Z0-9_.-]+`. Odrzucamy puste, znaki kontrolne
    /// i typowe wektory injection (slash, dwukropek, spacja).
    fn validate_container_id(id: &str) -> Result<(), String> {
        if id.is_empty() {
            return Err("container_id pusty".to_string());
        }
        if id.len() > 128 {
            return Err("container_id za dlugi".to_string());
        }
        let ok = id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-');
        if !ok {
            return Err("container_id zawiera niedozwolone znaki".to_string());
        }
        Ok(())
    }

    async fn handle_container_start(&self, container_id: &str) -> CommandResponse {
        if let Err(e) = Self::validate_container_id(container_id) {
            return CommandResponse::fail(e);
        }
        #[cfg(feature = "docker")]
        {
            let docker = match Self::connect_docker().await {
                Ok(d) => d,
                Err(e) => return CommandResponse::fail(e),
            };
            match docker
                .start_container(
                    container_id,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
            {
                Ok(()) => CommandResponse::ok(MeshCommandResponsePayload::Empty),
                Err(e) => CommandResponse::fail(format!("start_container: {}", e)),
            }
        }
        #[cfg(not(feature = "docker"))]
        {
            let _ = container_id;
            CommandResponse::fail("docker feature nie jest aktywne w tej kompilacji")
        }
    }

    async fn handle_container_stop(&self, container_id: &str) -> CommandResponse {
        if let Err(e) = Self::validate_container_id(container_id) {
            return CommandResponse::fail(e);
        }
        #[cfg(feature = "docker")]
        {
            let docker = match Self::connect_docker().await {
                Ok(d) => d,
                Err(e) => return CommandResponse::fail(e),
            };
            match docker.stop_container(container_id, None).await {
                Ok(()) => CommandResponse::ok(MeshCommandResponsePayload::Empty),
                Err(e) => CommandResponse::fail(format!("stop_container: {}", e)),
            }
        }
        #[cfg(not(feature = "docker"))]
        {
            let _ = container_id;
            CommandResponse::fail("docker feature nie jest aktywne w tej kompilacji")
        }
    }

    async fn handle_container_logs(&self, container_id: &str, tail_lines: u32) -> CommandResponse {
        if let Err(e) = Self::validate_container_id(container_id) {
            return CommandResponse::fail(e);
        }
        #[cfg(feature = "docker")]
        {
            let docker = match Self::connect_docker().await {
                Ok(d) => d,
                Err(e) => return CommandResponse::fail(e),
            };
            // `tail=0` means ALL lines for the docker daemon; a failed deploy
            // wants the last handful, so only 0 maps to "all".
            let tail = if tail_lines == 0 {
                "0".to_string()
            } else {
                tail_lines.to_string()
            };
            let opts = bollard::query_parameters::LogsOptionsBuilder::default()
                .stdout(true)
                .stderr(true)
                .follow(false)
                .tail(&tail)
                .build();
            use futures::StreamExt;
            let mut stream = docker.logs(container_id, Some(opts));
            let mut logs = String::new();
            while let Some(item) = stream.next().await {
                match item {
                    Ok(out) => {
                        let line = out.to_string();
                        let line = line.trim_end_matches(['\r', '\n']);
                        logs.push_str(line);
                        logs.push('\n');
                    }
                    Err(e) => {
                        // Partial logs are still diagnostic gold — return what we
                        // have instead of failing the whole fetch.
                        if logs.is_empty() {
                            return CommandResponse::fail(format!("logs: {}", e));
                        }
                        break;
                    }
                }
            }
            CommandResponse::ok(MeshCommandResponsePayload::ContainerLogsResult { logs })
        }
        #[cfg(not(feature = "docker"))]
        {
            let _ = (container_id, tail_lines);
            CommandResponse::fail("docker feature nie jest aktywne w tej kompilacji")
        }
    }

    async fn handle_container_restart(&self, container_id: &str) -> CommandResponse {
        if let Err(e) = Self::validate_container_id(container_id) {
            return CommandResponse::fail(e);
        }
        #[cfg(feature = "docker")]
        {
            let docker = match Self::connect_docker().await {
                Ok(d) => d,
                Err(e) => return CommandResponse::fail(e),
            };
            match docker.restart_container(container_id, None).await {
                Ok(()) => CommandResponse::ok(MeshCommandResponsePayload::Empty),
                Err(e) => CommandResponse::fail(format!("restart_container: {}", e)),
            }
        }
        #[cfg(not(feature = "docker"))]
        {
            let _ = container_id;
            CommandResponse::fail("docker feature nie jest aktywne w tej kompilacji")
        }
    }

    /// SystemPrune wola docker prune dla kontenerow + obrazow (oraz volumes
    /// jesli `volumes=true`). Zwraca text z laczna iloscia odzyskanej przestrzeni.
    async fn handle_system_prune(&self, volumes: bool) -> CommandResponse {
        #[cfg(feature = "docker")]
        {
            let docker = match Self::connect_docker().await {
                Ok(d) => d,
                Err(e) => return CommandResponse::fail(e),
            };

            let containers = match docker
                .prune_containers(None::<bollard::query_parameters::PruneContainersOptions>)
                .await
            {
                Ok(r) => r,
                Err(e) => return CommandResponse::fail(format!("prune_containers: {}", e)),
            };
            let images = match docker
                .prune_images(None::<bollard::query_parameters::PruneImagesOptions>)
                .await
            {
                Ok(r) => r,
                Err(e) => return CommandResponse::fail(format!("prune_images: {}", e)),
            };
            let volumes_resp = if volumes {
                match docker
                    .prune_volumes(None::<bollard::query_parameters::PruneVolumesOptions>)
                    .await
                {
                    Ok(r) => Some(r),
                    Err(e) => return CommandResponse::fail(format!("prune_volumes: {}", e)),
                }
            } else {
                None
            };

            let containers_count = containers
                .containers_deleted
                .as_ref()
                .map(|v| v.len())
                .unwrap_or(0);
            let containers_bytes = containers.space_reclaimed.unwrap_or(0);
            let images_count = images.images_deleted.as_ref().map(|v| v.len()).unwrap_or(0);
            let images_bytes = images.space_reclaimed.unwrap_or(0);
            let (volumes_count, volumes_bytes) = match volumes_resp {
                Some(v) => (
                    v.volumes_deleted.as_ref().map(|v| v.len()).unwrap_or(0),
                    v.space_reclaimed.unwrap_or(0),
                ),
                None => (0usize, 0i64),
            };

            let total_bytes = containers_bytes + images_bytes + volumes_bytes;
            let summary = format!(
                "Prune ok: containers={} ({} B), images={} ({} B), volumes={} ({} B), total reclaimed={} B",
                containers_count,
                containers_bytes,
                images_count,
                images_bytes,
                volumes_count,
                volumes_bytes,
                total_bytes
            );
            CommandResponse::ok(MeshCommandResponsePayload::Text(summary))
        }
        #[cfg(not(feature = "docker"))]
        {
            let _ = volumes;
            CommandResponse::fail("docker feature nie jest aktywne w tej kompilacji")
        }
    }
}

// Cross-node service action: after a remote-triggered mutation succeeds the
// receiver pushes a `MeshServicesUpdate` so every other peer's
// `MeshServicesRegistry` (including the original initiator) reflects the new
// state without waiting for the 5-min anti-drift announce.
async fn push_service_change_after_action(
    actions: &ServiceActionContext,
    local_node_id: &str,
    service_id: i64,
    removed: bool,
) {
    let change = if removed {
        Some(tentaflow_protocol::ServiceChange::Removed { service_id })
    } else {
        match crate::services::snapshot_builder::build_one(&actions.db, service_id, local_node_id) {
            Ok(Some(info)) => Some(tentaflow_protocol::ServiceChange::Updated(info)),
            Ok(None) => None,
            Err(e) => {
                warn!(error = %e, service_id, "MeshServicesUpdate (action result): build_one failed");
                None
            }
        }
    };
    let Some(change) = change else { return };
    let payload = tentaflow_protocol::mesh::MeshServicesUpdatePayload {
        from_node_id: local_node_id.to_string(),
        change,
    };
    if let Ok(bytes) = crate::mesh::cbor::encode(&payload) {
        let _ = actions
            .iroh
            .broadcast_ufp2_to_trusted(
                tentaflow_protocol::mesh::MESH_MSG_SERVICES_UPDATE,
                &bytes,
                None,
            )
            .await;
    }
}

/// Wire deploy method tag → internal `DeployMethod` variant. Mirrors the
/// helper in `dispatch::handlers` but kept private here so the executor does
/// not pull on the dispatch crate boundary.
fn resolve_deploy_method(
    manifest: &crate::services::manifest::ServiceManifest,
    method: &str,
) -> std::result::Result<crate::services_repo::services::DeployMethod, String> {
    use crate::services::manifest::NativeRuntime;
    use crate::services_repo::services::DeployMethod;
    match method {
        "docker" => Ok(DeployMethod::Docker),
        "external" => Ok(DeployMethod::External),
        "native" => {
            let native =
                manifest.deploy.native.as_ref().ok_or_else(|| {
                    format!("engine '{}' has no [deploy.native]", manifest.engine.id)
                })?;
            Ok(match native.runtime {
                NativeRuntime::Embedded => DeployMethod::NativeEmbedded,
                NativeRuntime::Binary => DeployMethod::NativeBinary,
                NativeRuntime::PythonBundle => DeployMethod::NativePythonBundle,
                NativeRuntime::ManagedCli => DeployMethod::NativeManagedCli,
            })
        }
        other => Err(format!(
            "unknown deploy method '{}': expected docker/native/external",
            other
        )),
    }
}

/// A single enabled robot-controlling addon instance candidate, distilled from
/// the addon row + parsed manifest so the selection logic in
/// [`select_robot_addon`] is pure and unit-testable without a DB.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RobotAddonCandidate {
    pub(crate) addon_id: String,
    pub(crate) package_id: String,
    pub(crate) max_velocity: f64,
    /// Robot kind from manifest `[robot].kind` ("quadruped", "drone", ...).
    /// Carried so the advertiser can publish it without re-parsing manifests.
    pub(crate) kind: Option<String>,
}

/// Enumerate this node's INSTALLED + ENABLED robot-controlling addons: every
/// addon whose manifest carries `[robot] controls_robot=true`. Single source of
/// truth for both the receiver's [`resolve_robot_addon`] (precise pick for one
/// `robot_id`) and the advertiser (publishes the full owned-robot list to the
/// mesh). Each entry carries the instance id, package/base id, kind and the
/// movement safety ceiling from `[robot.safety].max_linear_mps`.
pub(crate) fn collect_local_robot_addons(db: &DbPool) -> Vec<RobotAddonCandidate> {
    let Ok(addons) = crate::db::repository::list_addons(db) else {
        return Vec::new();
    };
    let mut candidates: Vec<RobotAddonCandidate> = Vec::new();
    for a in addons {
        if !a.is_enabled {
            continue;
        }
        // `manifest_json` holds the RAW manifest.toml string.
        let manifest = match crate::addon::lifecycle::parse_manifest_toml(&a.manifest_json) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let Some(robot) = manifest.robot.as_ref() else {
            continue;
        };
        if !robot.controls_robot {
            continue;
        }
        let max_velocity = robot
            .safety
            .as_ref()
            .and_then(|s| s.max_linear_mps)
            .unwrap_or(crate::mesh::robot_control::MAX_VELOCITY);
        candidates.push(RobotAddonCandidate {
            addon_id: a.addon_id,
            package_id: a.package_id,
            max_velocity,
            kind: robot.kind.clone(),
        });
    }
    candidates
}

/// Pure selection: pick the robot addon instance for `robot_id` from the set of
/// enabled `[robot] controls_robot=true` candidates. Precise + unambiguous:
///
/// 1. EXACT addon-instance-id match (`addon_id == robot_id`) wins — the sender is
///    expected to pass the concrete instance id, so this is unambiguous even with
///    several go2-* instances installed.
/// 2. Otherwise fall back to base/package-id match, but ONLY if exactly one
///    candidate matches. If 2+ match, return `None` (UnknownRobot) rather than
///    silently picking one — actuating the wrong robot would be unsafe.
///
/// Returns the resolved addon, plus a flag set when the package-id fallback was
/// abandoned due to ambiguity (so the caller can `warn!`).
pub(crate) fn select_robot_addon(
    candidates: &[RobotAddonCandidate],
    robot_id: &str,
) -> (Option<crate::mesh::robot_control::ResolvedRobotAddon>, bool) {
    if let Some(exact) = candidates.iter().find(|c| c.addon_id == robot_id) {
        return (
            Some(crate::mesh::robot_control::ResolvedRobotAddon {
                addon_id: exact.addon_id.clone(),
                max_velocity: exact.max_velocity,
            }),
            false,
        );
    }
    let mut by_package = candidates.iter().filter(|c| {
        let base = if c.package_id.is_empty() {
            c.addon_id.as_str()
        } else {
            c.package_id.as_str()
        };
        base == robot_id
    });
    match (by_package.next(), by_package.next()) {
        (Some(only), None) => (
            Some(crate::mesh::robot_control::ResolvedRobotAddon {
                addon_id: only.addon_id.clone(),
                max_velocity: only.max_velocity,
            }),
            false,
        ),
        (Some(_), Some(_)) => (None, true),
        _ => (None, false),
    }
}

/// Deny-by-default resolver for the local robot-control addon owning `robot_id`.
/// Enumerates INSTALLED + ENABLED addons, parses each manifest, keeps those
/// carrying a `[robot] controls_robot=true` block, then defers to the pure
/// [`select_robot_addon`] for the precise/unambiguous choice. The movement safety
/// ceiling comes from `[robot.safety].max_linear_mps` (falling back to the
/// protocol max). `None` → the caller rejects as UnknownRobot.
pub(crate) fn resolve_robot_addon(
    db: &DbPool,
    robot_id: &str,
) -> Option<crate::mesh::robot_control::ResolvedRobotAddon> {
    let candidates = collect_local_robot_addons(db);
    let (resolved, ambiguous) = select_robot_addon(&candidates, robot_id);
    if ambiguous {
        warn!(
            robot = %robot_id,
            "robot control: multiple enabled robot addons match the base/package id; \
             refusing to pick one (pass the concrete addon instance id as robot_id)"
        );
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_container_id_accepts_hex_and_names() {
        assert!(MeshCommandExecutor::validate_container_id("abcdef0123456789").is_ok());
        assert!(MeshCommandExecutor::validate_container_id("tentaflow-llm.0").is_ok());
        assert!(MeshCommandExecutor::validate_container_id("my_container").is_ok());
    }

    #[test]
    fn validate_container_id_rejects_injection_vectors() {
        assert!(MeshCommandExecutor::validate_container_id("").is_err());
        assert!(MeshCommandExecutor::validate_container_id("foo bar").is_err());
        assert!(MeshCommandExecutor::validate_container_id("foo/../bar").is_err());
        assert!(MeshCommandExecutor::validate_container_id("foo;rm -rf /").is_err());
        assert!(MeshCommandExecutor::validate_container_id("foo:bar").is_err());
        let long = "a".repeat(200);
        assert!(MeshCommandExecutor::validate_container_id(&long).is_err());
    }

    #[tokio::test]
    async fn container_start_rejects_invalid_id_without_docker_call() {
        let executor = create_test_executor();
        let resp = executor.handle_container_start("foo bar").await;
        assert!(!resp.ok);
        assert!(resp
            .error
            .unwrap_or_default()
            .contains("niedozwolone znaki"));
    }

    #[test]
    fn odrzuca_path_traversal() {
        let executor = create_test_executor();
        let result = executor.validate_target_dir("/tmp/../etc/shadow");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("poza dozwolonym katalogiem"));
    }

    #[test]
    fn odrzuca_sciezke_poza_dozwolonym_katalogiem() {
        let executor = create_test_executor();
        let result = executor.validate_target_dir("/tmp/certs");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("poza dozwolonym katalogiem"));
    }

    #[test]
    fn akceptuje_sciezke_w_tentaflow() {
        let executor = create_test_executor();
        let result = executor.validate_target_dir("~/.tentaflow/certs");
        if dirs::home_dir().is_some() {
            assert!(result.is_ok());
        }
    }

    /// Niezaufany peer dostaje `ok=false` z opisem bledu — wszystkie komendy
    /// (lacznie z profiling) sa odrzucane na samym wejsciu, niezaleznie od ich
    /// payloadu.
    #[tokio::test]
    async fn executor_rejects_untrusted_peer() {
        let executor = create_test_executor();
        let req = tentaflow_protocol::ProfilingSessionsRequest {
            node_id: "untrusted-peer".to_string(),
        };
        let resp = executor
            .execute("untrusted-peer", MeshCommandType::ProfilingSessions(req))
            .await;
        assert!(!resp.ok);
        let err = resp.error.unwrap_or_default();
        assert!(
            err.contains("nie jest zaufany"),
            "spodziewano sie komunikatu o trust, mam: {}",
            err
        );
    }

    fn robot_candidate(addon_id: &str, package_id: &str, max_v: f64) -> RobotAddonCandidate {
        RobotAddonCandidate {
            addon_id: addon_id.to_string(),
            package_id: package_id.to_string(),
            max_velocity: max_v,
            kind: None,
        }
    }

    #[test]
    fn select_robot_addon_exact_instance_id_wins() {
        // Two go2 instances; robot_id is the concrete instance id → exact match,
        // never ambiguous even though both share the go2 package id.
        let cands = vec![
            robot_candidate("go2-living-room", "go2", 0.5),
            robot_candidate("go2-garage", "go2", 0.8),
        ];
        let (resolved, ambiguous) = select_robot_addon(&cands, "go2-garage");
        assert!(!ambiguous);
        let resolved = resolved.expect("exact match");
        assert_eq!(resolved.addon_id, "go2-garage");
        assert_eq!(resolved.max_velocity, 0.8);
    }

    #[test]
    fn select_robot_addon_package_fallback_single_match() {
        // Only one go2 instance, addressed by package/base id → unambiguous fallback.
        let cands = vec![
            robot_candidate("go2-only", "go2", 0.5),
            robot_candidate("spot-1", "spot", 1.0),
        ];
        let (resolved, ambiguous) = select_robot_addon(&cands, "go2");
        assert!(!ambiguous);
        assert_eq!(resolved.expect("fallback").addon_id, "go2-only");
    }

    #[test]
    fn select_robot_addon_ambiguous_package_match_returns_none() {
        // Two enabled go2 instances addressed by package id → must NOT pick one.
        let cands = vec![
            robot_candidate("go2-a", "go2", 0.5),
            robot_candidate("go2-b", "go2", 0.8),
        ];
        let (resolved, ambiguous) = select_robot_addon(&cands, "go2");
        assert!(ambiguous, "two package matches must flag ambiguity");
        assert!(resolved.is_none(), "ambiguous fallback must deny");
    }

    #[test]
    fn select_robot_addon_no_match_is_unknown_robot() {
        let cands = vec![robot_candidate("spot-1", "spot", 1.0)];
        let (resolved, ambiguous) = select_robot_addon(&cands, "go2");
        assert!(!ambiguous);
        assert!(resolved.is_none());
    }

    #[test]
    fn select_robot_addon_empty_package_falls_back_to_addon_id() {
        // An instance with no package_id is matched by its addon_id as the base.
        let cands = vec![robot_candidate("go2", "", 0.5)];
        let (resolved, ambiguous) = select_robot_addon(&cands, "go2");
        assert!(!ambiguous);
        assert_eq!(resolved.expect("base match").addon_id, "go2");
    }

    fn create_test_executor() -> MeshCommandExecutor {
        let db = create_test_db();
        let settings_cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));
        let security = Arc::new(MeshSecurity::new(db, settings_cipher).unwrap());
        let tmp = std::env::temp_dir().join(format!(
            "tentaflow-mesh-cmd-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&tmp).expect("test data dir");
        MeshCommandExecutor::new(security, "test-node".to_string(), tmp)
    }

    fn create_test_db() -> crate::db::DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS trusted_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                public_key TEXT NOT NULL,
                hostname TEXT DEFAULT '',
                approved_by TEXT DEFAULT '',
                approved_at TEXT NOT NULL DEFAULT (datetime('now')),
                is_active INTEGER NOT NULL DEFAULT 1,
                last_addresses TEXT DEFAULT NULL,
                environment TEXT
            );
            CREATE TABLE IF NOT EXISTS pending_pairings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                remote_node_id TEXT NOT NULL,
                pin_code TEXT NOT NULL,
                direction TEXT NOT NULL CHECK(direction IN ('outgoing','incoming')),
                expires_at TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            ",
        )
        .unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }
}
