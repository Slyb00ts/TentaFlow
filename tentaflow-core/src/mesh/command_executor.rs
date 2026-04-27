// =============================================================================
// Plik: mesh/command_executor.rs
// Opis: Executor komend mesh — wykonuje komendy zarzadzania otrzymane od
//       zdalnych nodow. Sprawdza trust przed wykonaniem.
// =============================================================================

use std::path::PathBuf;
use std::sync::Arc;

use tracing::{info, warn};
use zeroize::Zeroize;

use crate::mesh::security::MeshSecurity;
use crate::profiling::{ProfileStorage, ProfilingError, NSYS_RUNNER};
use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType};

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
/// `local_node_id` i `data_dir` sa potrzebne do uruchamiania `ProfileStorage`
/// dla komend Nsight (storage ma layout `<data_dir>/nsight/<node_id>/...`).
pub struct MeshCommandExecutor {
    security: Arc<MeshSecurity>,
    local_node_id: String,
    data_dir: PathBuf,
}

impl MeshCommandExecutor {
    pub fn new(security: Arc<MeshSecurity>, local_node_id: String, data_dir: PathBuf) -> Self {
        Self {
            security,
            local_node_id,
            data_dir,
        }
    }

    fn profile_storage(&self) -> ProfileStorage {
        ProfileStorage::new(&self.data_dir, &self.local_node_id)
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
                    Ok(Ok(output)) => {
                        CommandResponse::ok(MeshCommandResponsePayload::Text(output))
                    }
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
            MeshCommandType::SystemPrune { volumes } => {
                self.handle_system_prune(volumes).await
            }

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
                        CommandResponse::ok(MeshCommandResponsePayload::BandwidthProbeServerStarted {
                            tcp_port,
                            rdma_port: server_rdma_port,
                        })
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

            MeshCommandType::NsightStart(req) => self.handle_nsight_start(req).await,
            MeshCommandType::NsightStop(req) => self.handle_nsight_stop(req).await,
            MeshCommandType::NsightSessions(req) => self.handle_nsight_sessions(req).await,
            MeshCommandType::NsightReport(req) => self.handle_nsight_report(req).await,
            MeshCommandType::NsightDelete(req) => self.handle_nsight_delete(req).await,
            MeshCommandType::NsightDownload(req) => self.handle_nsight_download(req).await,
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
    // Nsight handlery — wykonywane na nodzie odbierajacym komende mesh.
    // Dla local node ten sam kod jest wolany bezposrednio z dispatch handlera.
    // -------------------------------------------------------------------------

    async fn handle_nsight_start(
        &self,
        req: tentaflow_protocol::profiling::NsightStartRequest,
    ) -> CommandResponse {
        let storage = self.profile_storage();
        match NSYS_RUNNER
            .start(req.scope, req.duration_secs, req.label, &storage)
            .await
        {
            Ok((session_id, started_at_ms)) => CommandResponse::ok(
                MeshCommandResponsePayload::NsightStart(
                    tentaflow_protocol::profiling::NsightStartResponse {
                        session_id,
                        started_at_ms,
                    },
                ),
            ),
            Err(e) => CommandResponse::fail(format!("nsight start: {}", e)),
        }
    }

    async fn handle_nsight_stop(
        &self,
        req: tentaflow_protocol::profiling::NsightStopRequest,
    ) -> CommandResponse {
        let storage = self.profile_storage();
        match NSYS_RUNNER.stop(&req.session_id, &storage).await {
            Ok(status) => CommandResponse::ok(MeshCommandResponsePayload::NsightStop(
                tentaflow_protocol::profiling::NsightStopResponse {
                    session_id: req.session_id,
                    status,
                },
            )),
            Err(e) => CommandResponse::fail(format!("nsight stop: {}", e)),
        }
    }

    async fn handle_nsight_sessions(
        &self,
        req: tentaflow_protocol::profiling::NsightSessionsRequest,
    ) -> CommandResponse {
        let storage = self.profile_storage();
        match storage.list() {
            Ok(sessions) => CommandResponse::ok(MeshCommandResponsePayload::NsightSessions(
                tentaflow_protocol::profiling::NsightSessionsResponse {
                    node_id: req.node_id,
                    sessions,
                },
            )),
            Err(e) => CommandResponse::fail(format!("nsight sessions: {}", e)),
        }
    }

    async fn handle_nsight_report(
        &self,
        req: tentaflow_protocol::profiling::NsightReportRequest,
    ) -> CommandResponse {
        let storage = self.profile_storage();
        match storage.read_summary(&req.session_id) {
            Ok(report) => CommandResponse::ok(MeshCommandResponsePayload::NsightReport(
                tentaflow_protocol::profiling::NsightReportResponse { report },
            )),
            Err(ProfilingError::InvalidSessionId) => {
                CommandResponse::fail("invalid session id".to_string())
            }
            Err(ProfilingError::NotFound(s)) => {
                CommandResponse::fail(format!("session not found: {}", s))
            }
            Err(e) => CommandResponse::fail(format!("nsight report: {}", e)),
        }
    }

    async fn handle_nsight_delete(
        &self,
        req: tentaflow_protocol::profiling::NsightDeleteRequest,
    ) -> CommandResponse {
        let storage = self.profile_storage();
        match storage.delete(&req.session_id) {
            Ok(()) => CommandResponse::ok(MeshCommandResponsePayload::NsightDelete(
                tentaflow_protocol::profiling::NsightDeleteResponse {
                    session_id: req.session_id,
                    ok: true,
                },
            )),
            Err(ProfilingError::InvalidSessionId) => {
                CommandResponse::fail("invalid session id".to_string())
            }
            Err(ProfilingError::NotFound(s)) => {
                CommandResponse::fail(format!("session not found: {}", s))
            }
            Err(e) => CommandResponse::fail(format!("nsight delete: {}", e)),
        }
    }

    async fn handle_nsight_download(
        &self,
        req: tentaflow_protocol::profiling::NsightDownloadRequest,
    ) -> CommandResponse {
        let storage = self.profile_storage();
        let path = match storage.raw_report_path(&req.session_id) {
            Ok(p) => p,
            Err(ProfilingError::InvalidSessionId) => {
                return CommandResponse::fail("invalid session id".to_string());
            }
            Err(e) => return CommandResponse::fail(format!("nsight download: {}", e)),
        };
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let filename = format!("nsight-{}.nsys-rep", req.session_id);
                CommandResponse::ok(MeshCommandResponsePayload::NsightDownload(
                    tentaflow_protocol::profiling::NsightDownloadResponse {
                        session_id: req.session_id,
                        filename,
                        bytes,
                    },
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                CommandResponse::fail(format!("session not found: {}", req.session_id))
            }
            Err(e) => CommandResponse::fail(format!("nsight download: {}", e)),
        }
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
            let images_count = images
                .images_deleted
                .as_ref()
                .map(|v| v.len())
                .unwrap_or(0);
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
    /// (lacznie z Nsight) sa odrzucane na samym wejsciu, niezaleznie od ich
    /// payloadu.
    #[tokio::test]
    async fn executor_rejects_untrusted_peer() {
        let executor = create_test_executor();
        let req = tentaflow_protocol::profiling::NsightStartRequest {
            node_id: "untrusted-peer".to_string(),
            scope: tentaflow_protocol::profiling::NsightScope::Cpu,
            duration_secs: 5,
            label: String::new(),
        };
        let resp = executor
            .execute(
                "untrusted-peer",
                MeshCommandType::NsightStart(req),
            )
            .await;
        assert!(!resp.ok);
        let err = resp.error.unwrap_or_default();
        assert!(
            err.contains("nie jest zaufany"),
            "spodziewano sie komunikatu o trust, mam: {}",
            err
        );
    }

    /// Sessions list dla zaufanego peera dziala bez nsys w PATH (storage
    /// inicjalizuje sie ad-hoc, lista pusta przy nowym data_dir).
    #[tokio::test]
    async fn executor_dispatches_nsight_sessions_for_trusted_peer() {
        let executor = create_test_executor();
        let trusted_id = "0123456789abcdef0123456789abcdef";
        // Generujemy realny klucz publiczny przez druga instancje MeshSecurity —
        // unikamy duplikowania logiki konkatenacji Ed25519+X25519, ktora siedzi
        // w `MeshSecurity::public_key_hex`.
        let other_db = create_test_db();
        let other_cipher = Arc::new(crate::crypto::SettingsCipher::new(&[1u8; 32]));
        let other = MeshSecurity::new(other_db, other_cipher).unwrap();
        let pk_hex = other.public_key_hex();
        executor
            .security
            .add_trusted_key(trusted_id, &pk_hex, "test-host")
            .expect("add trusted");

        let req = tentaflow_protocol::profiling::NsightSessionsRequest {
            node_id: trusted_id.to_string(),
        };
        let resp = executor
            .execute(trusted_id, MeshCommandType::NsightSessions(req))
            .await;
        assert!(resp.ok, "expected ok, got error: {:?}", resp.error);
        match resp.payload {
            tentaflow_protocol::mesh::MeshCommandResponsePayload::NsightSessions(p) => {
                assert!(p.sessions.is_empty(), "swieze data_dir powinno byc puste");
            }
            other => panic!("nieoczekiwany payload: {:?}", other),
        }
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
