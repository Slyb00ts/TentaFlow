// =============================================================================
// Plik: mesh/command_executor.rs
// Opis: Executor komend mesh — wykonuje komendy zarzadzania otrzymane od
//       zdalnych nodow. Sprawdza trust przed wykonaniem.
// =============================================================================

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock as AsyncRwLock;
use tracing::{info, warn};
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
}

impl MeshCommandExecutor {
    pub fn new(security: Arc<MeshSecurity>, local_node_id: String, data_dir: PathBuf) -> Self {
        Self {
            security,
            local_node_id,
            data_dir,
            service_actions: AsyncRwLock::new(None),
        }
    }

    /// Inject the resources needed for cross-node service action handlers.
    /// Called once during startup after the supervisor and iroh manager are
    /// up. Subsequent calls overwrite the previous context.
    pub async fn set_service_action_context(&self, ctx: ServiceActionContext) {
        *self.service_actions.write().await = Some(ctx);
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
            return CommandResponse::fail(format!("Node {} nie jest zaufany", from_node_id));
        }

        info!(
            from = %from_node_id,
            command = ?command,
            "Wykonuje komende mesh"
        );

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
                self.handle_service_deploy_remote(&engine_id, &deploy_method, &config_json)
                    .await
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
        }
    }

    // ----- Cross-node service action handlers (krok N3b) -----

    async fn handle_service_delete_remote(&self, service_id: i64) -> CommandResponse {
        let actions = match self.service_action_ctx().await {
            Some(c) => c,
            None => return CommandResponse::fail("service action context not configured"),
        };
        let svc = {
            let conn = match actions.db.lock() {
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
        // Best-effort runtime stop, then drop the row regardless.
        let _ = crate::services::deploy::stop(&svc, actions.port_allocator.clone()).await;
        // Scoped lock: drop the MutexGuard before awaiting again.
        {
            let conn = match actions.db.lock() {
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
            let conn = match actions.db.lock() {
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

        let svc = {
            let conn = match actions.db.lock() {
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
            cfg_obj.insert("model_preset_id".into(), serde_json::Value::String(preset_id));
            cfg_obj.insert("model_repo".into(), serde_json::Value::Null);
        }
        if let Some(util) = gpu_memory_utilization {
            if let Some(num) = serde_json::Number::from_f64(util as f64) {
                cfg_obj.insert("gpu_memory_utilization".into(), serde_json::Value::Number(num));
            }
        }
        if let Some(v) = max_model_len {
            cfg_obj.insert("max_model_len".into(), serde_json::Value::Number(v.into()));
        }
        if let Some(v) = max_num_seqs {
            cfg_obj.insert("max_num_seqs".into(), serde_json::Value::Number(v.into()));
        }
        if let Some(v) = max_num_batched_tokens {
            cfg_obj.insert(
                "max_num_batched_tokens".into(),
                serde_json::Value::Number(v.into()),
            );
        }
        if let Some(dt) = kv_cache_dtype {
            cfg_obj.insert("kv_cache_dtype".into(), serde_json::Value::String(dt));
        }
        if let Some(b) = chunked_prefill {
            cfg_obj.insert("chunked_prefill".into(), serde_json::Value::Bool(b));
        }
        if let Some(args) = vllm_args_override {
            cfg_obj.insert("vllm_args".into(), serde_json::Value::String(args));
        }
        let new_config_json = match serde_json::to_string(&cfg) {
            Ok(s) => s,
            Err(e) => return CommandResponse::fail(format!("serialize config: {}", e)),
        };

        {
            let conn = match actions.db.lock() {
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
                tracing::warn!(
                    service_id,
                    "service_update_remote: stop failed: {}", e
                );
            }
            {
                let conn = match actions.db.lock() {
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
            let engine_id = svc.engine_id.clone();
            let deploy_method = svc.deploy_method;
            let cfg_json_for_task = new_config_json.clone();
            tokio::spawn(async move {
                match crate::services::deploy::respawn(
                    &engine_id,
                    deploy_method,
                    &cfg_json_for_task,
                    ports,
                )
                .await
                {
                    Ok(handle) => {
                        if let Ok(conn) = db.lock() {
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
                        if let Ok(conn) = db.lock() {
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

        // When pausing, mirror the local handler: actively stop the runtime
        // and clear runtime metadata so health checks don't keep flapping.
        if paused {
            let svc = {
                let conn = match actions.db.lock() {
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
                let conn = match actions.db.lock() {
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
            let conn = match actions.db.lock() {
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
        let svc = {
            let conn = match actions.db.lock() {
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
            let conn = match actions.db.lock() {
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
        )
        .await;

        let result = match respawn {
            Ok(handle) => {
                let conn = match actions.db.lock() {
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
                if let Ok(conn) = actions.db.lock() {
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

        let slug = uuid::Uuid::new_v4().to_string();
        let log_sender = crate::deploy::log_bus::sender_for(&slug);
        let db_clone = actions.db.clone();
        let port_alloc = actions.port_allocator.clone();
        let manifest_task = manifest.clone();
        let user_config_task = user_config.clone();
        let log_sender_task = log_sender.clone();
        let slug_task = slug.clone();
        let local_node_id_task = self.local_node_id.clone();
        let iroh_task = actions.iroh.clone();

        tokio::spawn(async move {
            let start_ms = crate::deploy::log_bus::now_ms();
            let result = crate::services::deploy::deploy(
                resolved,
                &manifest_task,
                &user_config_task,
                &port_alloc,
                &db_clone,
                Some(log_sender_task.clone()),
                Some(slug_task.clone()),
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
                            change: tentaflow_protocol::ServiceChange::Added(info),
                        };
                        if let Ok(bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&payload) {
                            let _ = iroh_task
                                .broadcast_to_trusted(
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
                        final_status: "failure".to_string(),
                        image_tag: String::new(),
                        container_name: String::new(),
                        error_message: err.to_string(),
                        duration_ms: crate::deploy::log_bus::now_ms() - start_ms,
                    });
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
    if let Ok(bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&payload) {
        let _ = actions
            .iroh
            .broadcast_to_trusted(
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
            })
        }
        other => Err(format!(
            "unknown deploy method '{}': expected docker/native/external",
            other
        )),
    }
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
        use std::sync::Mutex;
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
                last_addresses TEXT DEFAULT NULL
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
        Arc::new(Mutex::new(conn))
    }
}
