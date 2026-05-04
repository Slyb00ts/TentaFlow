// ============ File: services/deploy/docker.rs — docker-container deploy strategy ============
//
// Default transport is `sidecar_quic`: a Rust QUIC sidecar speaks to the
// container's native HTTP API on a host-mapped port. A `transport_explicit:
// "direct_http"` hint in `user_config` skips the sidecar and exposes the
// container's HTTP port directly (Phase 6 preview for engines like Ollama).
//
// This strategy compiles only with the `docker` feature. Without it the
// `DockerDeploy::new` factory returns a stub that always errors at prepare.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::Transaction;

#[cfg(feature = "docker")]
use std::path::PathBuf;

use super::{
    build_new_service, transport_hint, DeployError, DeployResult, DeployStrategy, LogSink,
    PreparedDeploy,
};
#[cfg(feature = "docker")]
use super::{
    category_tag, models_from_manifest, resolve_display_name, smart_health_probe, RuntimeHandle,
    SmartProbeConfig, SmartProbeOutcome,
};
use crate::services::manifest::{DockerTransport, ServiceManifest};
use crate::services::ports::PortAllocator;
use crate::services::transport::Transport;
#[cfg(feature = "docker")]
use crate::services_repo::services::DeployMethod;
use crate::services_repo::services::{self as services_repo, ServiceStatus};

pub struct DockerDeploy {
    manifest: ServiceManifest,
    user_config: serde_json::Value,
    ports: Arc<PortAllocator>,
    #[cfg_attr(not(feature = "docker"), allow(dead_code))]
    log_sink: Option<LogSink>,
    #[cfg_attr(not(feature = "docker"), allow(dead_code))]
    container_id: std::sync::Mutex<Option<String>>,
}

impl DockerDeploy {
    pub fn new(
        manifest: ServiceManifest,
        user_config: serde_json::Value,
        ports: Arc<PortAllocator>,
        log_sink: Option<LogSink>,
    ) -> Self {
        Self {
            manifest,
            user_config,
            ports,
            log_sink,
            container_id: std::sync::Mutex::new(None),
        }
    }

    /// Resolves the runtime transport for this docker deploy.
    ///
    /// Source of truth (Phase 6): the manifest's `[deploy.docker].transport`
    /// field — `sidecar-quic` or `direct-http`. The legacy `transport_explicit`
    /// hint in `user_config` is honoured as an override only when set, which
    /// keeps existing wizard requests working until the GUI stops sending it.
    #[cfg_attr(not(feature = "docker"), allow(dead_code))]
    fn pick_transport(&self) -> Transport {
        if let Some(hint) = transport_hint(&self.user_config) {
            return match hint.as_str() {
                "direct_http" | "direct-http" => Transport::HttpDirect,
                _ => Transport::SidecarQuic,
            };
        }
        match self
            .manifest
            .deploy
            .docker
            .as_ref()
            .and_then(|d| d.transport)
        {
            Some(DockerTransport::DirectHttp) => Transport::HttpDirect,
            Some(DockerTransport::SidecarQuic) | None => Transport::SidecarQuic,
        }
    }
}

#[cfg(feature = "docker")]
mod backend {
    use super::*;
    use bollard::models::{ContainerCreateBody, HostConfig, PortBinding};
    use bollard::query_parameters::{
        BuildImageOptions, CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
    };
    use bollard::Docker;
    use std::collections::HashMap;
    use std::path::Path;

    pub(super) async fn connect() -> DeployResult<Docker> {
        Docker::connect_with_local_defaults()
            .map_err(|e| DeployError::Docker(format!("connect: {}", e)))
    }

    pub(super) async fn ping(docker: &Docker) -> DeployResult<()> {
        docker
            .ping()
            .await
            .map(|_| ())
            .map_err(|e| DeployError::Docker(format!("ping: {}", e)))
    }

    /// Returns true when a tagged image is already present locally.
    pub(super) async fn image_exists(docker: &Docker, tag: &str) -> DeployResult<bool> {
        match docker.inspect_image(tag).await {
            Ok(_) => Ok(true),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(false),
            Err(e) => Err(DeployError::Docker(format!(
                "inspect_image({}): {}",
                tag, e
            ))),
        }
    }

    /// Builds an image from `<containers_root>/<context_path>/`. Streams build
    /// log lines into `log` (when present). The Dockerfile is expected at
    /// `<context>/Dockerfile`; bollard reads it relative to the tar root, so
    /// we pack the context directory itself (not its parent) and point at
    /// `Dockerfile`.
    pub(super) async fn build_image_from_context(
        docker: &Docker,
        context: &Path,
        tag: &str,
        log: Option<&LogSink>,
    ) -> DeployResult<()> {
        use futures::StreamExt;

        if !context.is_dir() {
            return Err(DeployError::Manifest(format!(
                "docker context not a directory: {}",
                context.display()
            )));
        }

        // Pack the context root as the tar root so the Dockerfile is at the
        // top level. Bollard streams this body as the build context.
        let mut tar_builder = tar::Builder::new(Vec::new());
        tar_builder
            .append_dir_all(".", context)
            .map_err(|e| DeployError::Docker(format!("tar context: {}", e)))?;
        let tar_bytes = tar_builder
            .into_inner()
            .map_err(|e| DeployError::Docker(format!("tar finalize: {}", e)))?;

        let opts = BuildImageOptions {
            dockerfile: "Dockerfile".to_string(),
            t: Some(tag.to_string()),
            rm: true,
            ..Default::default()
        };

        use bollard::body_full;
        use hyper::body::Bytes;
        let body = body_full(Bytes::from(tar_bytes));
        let mut stream = docker.build_image(opts, None, Some(body));
        while let Some(item) = stream.next().await {
            match item {
                Ok(info) => {
                    if let Some(line) = info.stream {
                        let trimmed = line.trim_end();
                        if !trimmed.is_empty() {
                            if let Some(s) = log {
                                s.info(&format!("[docker build] {}", trimmed));
                            } else {
                                tracing::info!(target: "docker_build", "{}", trimmed);
                            }
                        }
                    }
                    if let Some(err_detail) = info.error_detail {
                        return Err(DeployError::Docker(format!(
                            "build error: {}",
                            err_detail.message.unwrap_or_default()
                        )));
                    }
                }
                Err(e) => return Err(DeployError::Docker(format!("build stream: {}", e))),
            }
        }
        Ok(())
    }

    /// Creates and starts a container. Returns container id.
    /// `binds` entries: (host_path, container_path, read_only).
    pub(super) async fn run(
        docker: &Docker,
        image: &str,
        name: &str,
        ports: &[(u16, u16, &str)], // (host, container, proto: "tcp"|"udp")
        env: &HashMap<String, String>,
        binds: &[(PathBuf, String, bool)],
        labels: &HashMap<String, String>,
    ) -> DeployResult<String> {
        let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
        let mut exposed: Vec<String> = Vec::new();
        for (host, ctr, proto) in ports {
            let key = format!("{}/{}", ctr, proto);
            port_bindings.insert(
                key.clone(),
                Some(vec![PortBinding {
                    host_ip: Some("0.0.0.0".into()),
                    host_port: Some(host.to_string()),
                }]),
            );
            exposed.push(key);
        }

        let env_vec: Vec<String> = env.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        let binds_vec: Vec<String> = binds
            .iter()
            .map(|(h, c, ro)| {
                if *ro {
                    format!("{}:{}:ro", h.display(), c)
                } else {
                    format!("{}:{}", h.display(), c)
                }
            })
            .collect();

        let host_config = HostConfig {
            port_bindings: Some(port_bindings),
            binds: if binds_vec.is_empty() {
                None
            } else {
                Some(binds_vec)
            },
            ..Default::default()
        };
        let body = ContainerCreateBody {
            image: Some(image.into()),
            env: if env_vec.is_empty() {
                None
            } else {
                Some(env_vec)
            },
            exposed_ports: if exposed.is_empty() {
                None
            } else {
                Some(exposed)
            },
            labels: if labels.is_empty() {
                None
            } else {
                Some(labels.clone())
            },
            host_config: Some(host_config),
            ..Default::default()
        };
        let opts = CreateContainerOptions {
            name: Some(name.into()),
            platform: String::new(),
        };
        // Best-effort cleanup of an old container with the same name.
        let _ = docker
            .remove_container(
                name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        let created = docker
            .create_container(Some(opts), body)
            .await
            .map_err(|e| DeployError::Docker(format!("create: {}", e)))?;
        docker
            .start_container(name, None::<StartContainerOptions>)
            .await
            .map_err(|e| DeployError::Docker(format!("start: {}", e)))?;
        Ok(created.id)
    }

    pub(super) async fn stop_and_remove(docker: &Docker, id: &str) -> DeployResult<()> {
        let _ = docker.stop_container(id, None).await;
        docker
            .remove_container(
                id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| DeployError::Docker(format!("remove: {}", e)))?;
        Ok(())
    }
}

#[async_trait]
impl DeployStrategy for DockerDeploy {
    #[cfg(feature = "docker")]
    async fn prepare(&mut self) -> DeployResult<PreparedDeploy> {
        use std::collections::HashMap;
        let docker_section = self.manifest.deploy.docker.as_ref().ok_or_else(|| {
            DeployError::Manifest(format!(
                "engine '{}' has no [deploy.docker]",
                self.manifest.engine.id
            ))
        })?;
        let context_path = docker_section.context_path.as_deref().ok_or_else(|| {
            DeployError::Manifest(
                "docker deploy needs context_path (compose not yet handled in v2)".into(),
            )
        })?;

        let docker = backend::connect().await?;
        backend::ping(&docker).await?;

        // Resolve build context against the extracted containers tree.
        let context_dir = crate::paths::containers_root().join(context_path);
        if !context_dir.exists() {
            return Err(DeployError::Manifest(format!(
                "docker context_path does not exist: {}",
                context_dir.display()
            )));
        }
        let image_tag = format!(
            "tentaflow/{}:{}",
            self.manifest.engine.id, self.manifest.engine.version
        );

        // Build only when missing — repeated deploys reuse the cached image.
        if !backend::image_exists(&docker, &image_tag).await? {
            if let Some(s) = &self.log_sink {
                s.info(&format!(
                    "[docker] building image {} from {}",
                    image_tag,
                    context_dir.display()
                ));
            }
            backend::build_image_from_context(
                &docker,
                &context_dir,
                &image_tag,
                self.log_sink.as_ref(),
            )
            .await?;
        }

        let transport = self.pick_transport();
        let internal_port = self.manifest.engine.default_port;
        let mut allocated = Vec::new();

        // Allocate ports.
        let (host_http, sidecar_quic) = if transport == Transport::SidecarQuic {
            let pair = self
                .ports
                .acquire_many(2)
                .map_err(|e| DeployError::PortAlloc(e.to_string()))?;
            allocated.extend_from_slice(&pair);
            (pair[0], Some(pair[1]))
        } else {
            let p = self
                .ports
                .acquire()
                .map_err(|e| DeployError::PortAlloc(e.to_string()))?;
            allocated.push(p);
            (p, None)
        };

        // Build env / labels.
        let mut env = super::standard_engine_env();
        env.insert("PORT".into(), internal_port.to_string());

        let mut labels = HashMap::new();
        labels.insert(
            "tentaflow.engine_id".to_string(),
            self.manifest.engine.id.clone(),
        );

        let mut port_map = vec![(host_http, internal_port, "tcp")];
        if let Some(q) = sidecar_quic {
            port_map.push((q, q, "udp"));
        }

        let container_name = format!("tentaflow-{}-{}", self.manifest.engine.id, host_http);
        if let Some(s) = &self.log_sink {
            s.info(&format!(
                "[docker] starting container '{}' image={} host_port={}",
                container_name, image_tag, host_http
            ));
        }

        // Mount the shared host models cache so HF / Torch downloads from a
        // Docker engine end up in the same place as native deploys. Read-write
        // because the container is the one populating the cache.
        let models_host = crate::paths::models_root();
        let _ = std::fs::create_dir_all(&models_host);
        let binds = vec![(
            models_host,
            crate::paths::CONTAINER_MODELS_PATH.to_string(),
            false,
        )];

        let id = backend::run(
            &docker,
            &image_tag,
            &container_name,
            &port_map,
            &env,
            &binds,
            &labels,
        )
        .await?;

        // Save id for rollback.
        if let Ok(mut slot) = self.container_id.lock() {
            *slot = Some(id.clone());
        }
        if let Some(s) = &self.log_sink {
            s.info(&format!(
                "[docker] container '{}' started (id={})",
                container_name,
                &id[..id.len().min(12)]
            ));
        }

        // Stream container logs into the dashboard sink. Background task
        // ends when the container stops or the daemon closes the stream.
        {
            let docker_for_logs = docker.clone();
            let name_for_logs = container_name.clone();
            let sink = self.log_sink.clone();
            tokio::spawn(async move {
                use futures::StreamExt;
                let opts = bollard::query_parameters::LogsOptionsBuilder::default()
                    .follow(true)
                    .stdout(true)
                    .stderr(true)
                    .tail("0")
                    .build();
                let mut stream = docker_for_logs.logs(&name_for_logs, Some(opts));
                while let Some(item) = stream.next().await {
                    if let Ok(out) = item {
                        let line = out.to_string();
                        let trimmed = line.trim_end();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Some(s) = &sink {
                            s.emit("log", trimmed);
                        }
                    }
                }
            });
        }

        // Smart probe: race readiness URLs forever, abort only on
        // container exit.
        let probe_cfg = SmartProbeConfig {
            readiness_urls: vec![
                format!("http://127.0.0.1:{}/v1/models", host_http),
                format!("http://127.0.0.1:{}/health", host_http),
            ],
            status_report_interval: std::time::Duration::from_secs(30),
            log_sink: self.log_sink.clone(),
        };
        let docker_for_probe = docker.clone();
        let name_for_probe = container_name.clone();
        let outcome = smart_health_probe(probe_cfg, move || {
            let d = docker_for_probe.clone();
            let n = name_for_probe.clone();
            async move {
                match d.inspect_container(&n, None).await {
                    Ok(info) => {
                        let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
                        if running {
                            None
                        } else {
                            // Exited — surface the exit code if Docker
                            // reported one.
                            let code = info
                                .state
                                .as_ref()
                                .and_then(|s| s.exit_code)
                                .map(|c| c as i32);
                            Some(code)
                        }
                    }
                    // Inspect failed — likely the container vanished.
                    Err(_) => Some(None),
                }
            }
        })
        .await;

        match outcome {
            SmartProbeOutcome::Ready => {}
            SmartProbeOutcome::ProcessExited(code) => {
                if let Some(s) = &self.log_sink {
                    s.info(&format!(
                        "[docker] container '{}' exited{} before becoming ready",
                        container_name,
                        code.map(|c| format!(" (code {})", c)).unwrap_or_default()
                    ));
                }
                let _ = backend::stop_and_remove(&docker, &id).await;
                for p in &allocated {
                    let _ = self.ports.release(*p);
                }
                return Err(DeployError::Spawn(format!(
                    "container '{}' exited before readiness",
                    container_name
                )));
            }
        }

        let endpoint_url = match transport {
            Transport::SidecarQuic => Some(format!("quic://127.0.0.1:{}", sidecar_quic.unwrap())),
            Transport::HttpDirect => Some(format!("http://127.0.0.1:{}", host_http)),
            _ => None,
        };

        let runtime = RuntimeHandle {
            pid: None,
            port: Some(host_http),
            sidecar_port: sidecar_quic,
            endpoint_url,
            container_id: Some(id),
            instance_dir: None,
        };
        let models = models_from_manifest(&self.manifest, &self.user_config);
        let config_json = serde_json::to_string(&self.user_config)
            .map_err(|e| DeployError::Other(format!("serialize config: {}", e)))?;

        Ok(PreparedDeploy {
            engine_id: self.manifest.engine.id.clone(),
            category: category_tag(&self.manifest).to_string(),
            display_name: resolve_display_name(&self.manifest),
            deploy_method: DeployMethod::Docker,
            transport,
            runtime,
            models,
            config_json,
            allocated_ports: allocated,
        })
    }

    #[cfg(not(feature = "docker"))]
    async fn prepare(&mut self) -> DeployResult<PreparedDeploy> {
        Err(DeployError::Docker(
            "tentaflow-core compiled without `docker` feature".into(),
        ))
    }

    fn commit(&self, tx: &Transaction<'_>, prepared: &PreparedDeploy) -> DeployResult<i64> {
        let new = build_new_service(prepared, ServiceStatus::Running);
        Ok(services_repo::insert_in_tx(tx, &new)?)
    }

    #[cfg(feature = "docker")]
    async fn rollback(&self, prepared: PreparedDeploy) -> DeployResult<()> {
        let id = self
            .container_id
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(id) = id {
            if let Ok(docker) = backend::connect().await {
                let _ = backend::stop_and_remove(&docker, &id).await;
            }
        }
        for p in &prepared.allocated_ports {
            let _ = self.ports.release(*p);
        }
        Ok(())
    }

    #[cfg(not(feature = "docker"))]
    async fn rollback(&self, prepared: PreparedDeploy) -> DeployResult<()> {
        for p in &prepared.allocated_ports {
            let _ = self.ports.release(*p);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn skeleton_manifest(id: &str) -> ServiceManifest {
        use crate::services::manifest::{
            ApiKind, Category, DeploySection, DockerDeploy as DockerSec, DockerTransport, Engine,
            TargetOs,
        };
        ServiceManifest {
            engine: Engine {
                id: id.into(),
                category: Category::Llm,
                name: id.into(),
                description_pl: "".into(),
                description_en: "".into(),
                homepage: "".into(),
                license: "".into(),
                icon: None,
                resource_kind: None,
                requires_model: None,
                gpu_supported: None,
                default_port: 8000,
                api: ApiKind::OpenaiCompatible,
                version: "0".into(),
                service_surfaces: None,
                input_modalities: None,
                output_modalities: None,
            },
            deploy: DeploySection {
                docker: Some(DockerSec {
                    context_path: Some("/nonexistent/ctx".into()),
                    compose_path: None,
                    platforms: vec![TargetOs::Linux, TargetOs::Macos, TargetOs::Windows],
                    download_image: None,
                    download_size_mb: None,
                    transport: Some(DockerTransport::SidecarQuic),
                }),
                native: None,
                external: None,
            },
            model_presets: vec![],
            docker_source_hash: String::new(),
            native_source_hash: String::new(),
        }
    }

    /// Without the `docker` feature compiled in, prepare must return an error.
    #[cfg(not(feature = "docker"))]
    #[tokio::test]
    async fn prepare_errors_without_docker_feature() {
        let m = skeleton_manifest("no-docker");
        let ports = Arc::new(PortAllocator::new((48_500, 48_510), HashSet::new()).unwrap());
        let mut s = DockerDeploy::new(m, serde_json::json!({}), ports, None);
        let err = s.prepare().await.unwrap_err();
        assert!(matches!(err, DeployError::Docker(_)));
    }

    #[test]
    fn pick_transport_default_is_sidecar_quic() {
        let m = skeleton_manifest("def");
        let ports = Arc::new(PortAllocator::new((48_600, 48_610), HashSet::new()).unwrap());
        let s = DockerDeploy::new(m, serde_json::json!({}), ports, None);
        assert_eq!(s.pick_transport(), Transport::SidecarQuic);
    }

    #[test]
    fn pick_transport_honors_direct_http_hint() {
        let m = skeleton_manifest("hint");
        let ports = Arc::new(PortAllocator::new((48_700, 48_710), HashSet::new()).unwrap());
        let s = DockerDeploy::new(
            m,
            serde_json::json!({"transport_explicit": "direct_http"}),
            ports,
            None,
        );
        assert_eq!(s.pick_transport(), Transport::HttpDirect);
    }

    /// Live docker test — gated on a running daemon. Skipped silently when
    /// docker isn't reachable (CI without privileges, sandboxed builds).
    #[cfg(feature = "docker")]
    #[tokio::test]
    #[ignore]
    async fn docker_daemon_reachable_for_live_tests() {
        let docker = match super::backend::connect().await {
            Ok(d) => d,
            Err(_) => return,
        };
        let _ = super::backend::ping(&docker).await;
    }
}
