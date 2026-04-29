// ============ File: services/deploy/docker.rs — docker-container deploy strategy ============
//
// Default transport is `sidecar_quic`: a Rust QUIC sidecar speaks to the
// container's native HTTP API on a host-mapped port. A `transport_explicit:
// "direct_http"` hint in `user_config` skips the sidecar and exposes the
// container's HTTP port directly (Phase 6 preview for engines like Ollama).
//
// This strategy compiles only with the `docker` feature. Without it the
// `DockerDeploy::new` factory returns a stub that always errors at prepare.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::Transaction;

use super::{
    build_new_service, models_from_manifest, transport_hint, DeployError, DeployResult,
    DeployStrategy, PreparedDeploy, RuntimeHandle,
};
use crate::services::manifest::ServiceManifest;
use crate::services::ports::PortAllocator;
use crate::services::transport::Transport;
use crate::services_repo::services::{self as services_repo, DeployMethod, ServiceStatus};

pub struct DockerDeploy {
    manifest: ServiceManifest,
    user_config: serde_json::Value,
    ports: Arc<PortAllocator>,
    container_id: std::sync::Mutex<Option<String>>,
}

impl DockerDeploy {
    pub fn new(
        manifest: ServiceManifest,
        user_config: serde_json::Value,
        ports: Arc<PortAllocator>,
    ) -> Self {
        Self {
            manifest,
            user_config,
            ports,
            container_id: std::sync::Mutex::new(None),
        }
    }

    /// Selects the transport based on user_config hint. `sidecar_quic` is
    /// the default; `direct_http` bypasses the sidecar.
    fn pick_transport(&self) -> Transport {
        match transport_hint(&self.user_config).as_deref() {
            Some("direct_http") => Transport::HttpDirect,
            _ => Transport::SidecarQuic,
        }
    }
}

#[cfg(feature = "docker")]
mod backend {
    use super::*;
    use bollard::models::{ContainerCreateBody, HostConfig, PortBinding};
    use bollard::query_parameters::{
        CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
    };
    use bollard::Docker;
    use std::collections::HashMap;

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

    /// Creates and starts a container. Returns container id.
    pub(super) async fn run(
        docker: &Docker,
        image: &str,
        name: &str,
        ports: &[(u16, u16, &str)], // (host, container, proto: "tcp"|"udp")
        env: &HashMap<String, String>,
        binds: &[(PathBuf, String)],
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
            .map(|(h, c)| format!("{}:{}:ro", h.display(), c))
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

        // Phase 2: assume the image is already present (built by the legacy
        // path, by `docker pull`, or out-of-band). If it's missing, the run
        // step below fails with a clear `Docker(...)` error, and the user is
        // expected to build it — wiring the bundle build pipeline here is
        // tracked for Phase 5/6 once the legacy path is dismantled.
        let image_tag = format!(
            "tentaflow/{}:{}",
            self.manifest.engine.id, self.manifest.engine.version
        );
        let _ = context_path; // path validation is delegated to image-build phase

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
        let id = backend::run(
            &docker,
            &image_tag,
            &container_name,
            &port_map,
            &env,
            &[],
            &labels,
        )
        .await?;

        // Save id for rollback.
        if let Ok(mut slot) = self.container_id.lock() {
            *slot = Some(id.clone());
        }

        // Health check on the host-mapped HTTP port.
        let url_a = format!("http://127.0.0.1:{}/v1/models", host_http);
        let url_b = format!("http://127.0.0.1:{}/health", host_http);
        let res = wait_either(&url_a, &url_b, 60).await;
        if let Err(e) = res {
            let _ = backend::stop_and_remove(&docker, &id).await;
            for p in &allocated {
                let _ = self.ports.release(*p);
            }
            return Err(e);
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
        let models = models_from_manifest(&self.manifest);
        let config_json = serde_json::to_string(&self.user_config)
            .map_err(|e| DeployError::Other(format!("serialize config: {}", e)))?;

        Ok(PreparedDeploy {
            engine_id: self.manifest.engine.id.clone(),
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

#[cfg(feature = "docker")]
async fn wait_either(a: &str, b: &str, timeout_secs: u64) -> DeployResult<()> {
    use tokio::select;
    let fa = super::http_health_wait(a, timeout_secs);
    let fb = super::http_health_wait(b, timeout_secs);
    tokio::pin!(fa);
    tokio::pin!(fb);
    select! {
        r = &mut fa => r,
        r = &mut fb => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn skeleton_manifest(id: &str) -> ServiceManifest {
        use crate::services::manifest::{
            ApiKind, Category, DeploySection, DockerDeploy as DockerSec, Engine, TargetOs,
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
            },
            deploy: DeploySection {
                docker: Some(DockerSec {
                    context_path: Some("/nonexistent/ctx".into()),
                    compose_path: None,
                    platforms: vec![TargetOs::Linux, TargetOs::Macos, TargetOs::Windows],
                    download_image: None,
                    download_size_mb: None,
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
        let mut s = DockerDeploy::new(m, serde_json::json!({}), ports);
        let err = s.prepare().await.unwrap_err();
        assert!(matches!(err, DeployError::Docker(_)));
    }

    #[test]
    fn pick_transport_default_is_sidecar_quic() {
        let m = skeleton_manifest("def");
        let ports = Arc::new(PortAllocator::new((48_600, 48_610), HashSet::new()).unwrap());
        let s = DockerDeploy::new(m, serde_json::json!({}), ports);
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
