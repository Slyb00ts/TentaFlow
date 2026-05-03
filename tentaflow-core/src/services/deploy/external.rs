// ============ File: services/deploy/external.rs — deploy strategy for externally-managed daemons (Ollama, ...) ============
//
// `runtime = "external"` means the daemon is owned by the user (installed and
// started outside tentaflow). Our job is to probe it, persist a `services` row
// pointing at its endpoint, and keep using it like any other engine. There is
// no process to spawn and no port to allocate — rollback is a no-op because we
// did not start the daemon and must not stop it.

use std::time::Duration;

use async_trait::async_trait;
use rusqlite::Transaction;

use super::{
    build_new_service, category_tag, host_os_supported, models_from_manifest, resolve_display_name,
    DeployError, DeployResult, DeployStrategy, LogSink, PreparedDeploy, RuntimeHandle,
};
use crate::services::manifest::ServiceManifest;
use crate::services::transport::Transport;
use crate::services_repo::services::{self as services_repo, DeployMethod, ServiceStatus};

pub struct ExternalDeploy {
    manifest: ServiceManifest,
    user_config: serde_json::Value,
    log_sink: Option<LogSink>,
}

impl ExternalDeploy {
    pub fn new(
        manifest: ServiceManifest,
        user_config: serde_json::Value,
        log_sink: Option<LogSink>,
    ) -> Self {
        Self {
            manifest,
            user_config,
            log_sink,
        }
    }
}

#[async_trait]
impl DeployStrategy for ExternalDeploy {
    async fn prepare(&mut self) -> DeployResult<PreparedDeploy> {
        let external = self.manifest.deploy.external.as_ref().ok_or_else(|| {
            DeployError::Manifest(format!(
                "engine '{}' has no [deploy.external] section",
                self.manifest.engine.id
            ))
        })?;

        if !host_os_supported(&external.platforms) {
            return Err(DeployError::Manifest(format!(
                "engine '{}' [deploy.external] is not supported on the host OS",
                self.manifest.engine.id
            )));
        }

        // Resolve daemon URL: explicit override from user_config wins so ops can
        // point at a non-default port; fall back to the manifest's declared endpoint.
        let endpoint_url = self
            .user_config
            .get("detected_url")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| external.detection_endpoint.clone());

        let health_url = format!(
            "{}{}",
            endpoint_url.trim_end_matches('/'),
            external.detection_health_path
        );

        if let Some(s) = &self.log_sink {
            s.info(&format!(
                "[external] probing daemon at {} (health={})",
                endpoint_url, health_url
            ));
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| DeployError::Other(format!("reqwest builder: {}", e)))?;

        // Three quick attempts with a short backoff — covers daemons that are
        // mid-restart but already up in PATH.
        let mut last_err: Option<String> = None;
        for attempt in 1..=3u8 {
            match client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    last_err = None;
                    break;
                }
                Ok(resp) => last_err = Some(format!("status {}", resp.status())),
                Err(e) => last_err = Some(e.to_string()),
            }
            if attempt < 3 {
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
        if let Some(err) = last_err {
            return Err(DeployError::Other(format!(
                "external daemon unreachable at {}: {}",
                health_url, err
            )));
        }

        if let Some(s) = &self.log_sink {
            s.info("[external] daemon reachable, registering service row");
        }

        let models = models_from_manifest(&self.manifest, &self.user_config);
        let config_json = serde_json::to_string(&self.user_config)
            .map_err(|e| DeployError::Other(format!("serialize config: {}", e)))?;

        let runtime = RuntimeHandle {
            endpoint_url: Some(endpoint_url),
            ..Default::default()
        };

        Ok(PreparedDeploy {
            engine_id: self.manifest.engine.id.clone(),
            category: category_tag(&self.manifest).to_string(),
            display_name: resolve_display_name(&self.manifest),
            deploy_method: DeployMethod::External,
            transport: Transport::ExternalHttp,
            runtime,
            models,
            config_json,
            allocated_ports: Vec::new(),
        })
    }

    fn commit(&self, tx: &Transaction<'_>, prepared: &PreparedDeploy) -> DeployResult<i64> {
        let new = build_new_service(prepared, ServiceStatus::Running);
        let id = services_repo::insert_in_tx(tx, &new)?;
        Ok(id)
    }

    async fn rollback(&self, _prepared: PreparedDeploy) -> DeployResult<()> {
        // Daemon is user-owned; never touch its lifecycle on rollback.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::manifest::{
        ApiKind, Category, DeploySection, Engine, ExternalDeploy as ManifestExternal, ModelPreset,
        TargetOs,
    };

    fn manifest_with_external(id: &str, endpoint: &str, health: &str) -> ServiceManifest {
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
                default_port: 11434,
                api: ApiKind::OpenaiCompatible,
                version: "0".into(),
                service_surfaces: None,
                input_modalities: None,
                output_modalities: None,
            },
            deploy: DeploySection {
                docker: None,
                native: None,
                external: Some(ManifestExternal {
                    platforms: vec![TargetOs::Linux, TargetOs::Macos, TargetOs::Windows],
                    detection_binary: id.into(),
                    detection_endpoint: endpoint.into(),
                    detection_health_path: health.into(),
                }),
            },
            model_presets: vec![ModelPreset {
                id: "preset-x".into(),
                display_name: "Preset X".into(),
                repo: "x".into(),
                quantization: None,
                recommended: true,
                service_surfaces: None,
                input_modalities: None,
                output_modalities: None,
            }],
            docker_source_hash: String::new(),
            native_source_hash: String::new(),
        }
    }

    #[tokio::test]
    async fn prepare_fails_when_endpoint_unreachable() {
        // Use a port we know is not bound. The probe must fail within ~1s.
        let m = manifest_with_external("ext-down", "http://127.0.0.1:1", "/health");
        let mut s = ExternalDeploy::new(m, serde_json::json!({}), None);
        let err = s.prepare().await.unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("unreachable") || msg.contains("status"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn prepare_succeeds_against_local_mock() {
        // Spin up a tiny axum responder and point the manifest at it.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            // Accept exactly one request and reply 200 OK.
            let (mut sock, _) = listener.accept().await.unwrap();
            // Drain request bytes (best-effort)
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
            let body = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
            let _ = tokio::io::AsyncWriteExt::write_all(&mut sock, body).await;
        });

        let endpoint = format!("http://{}", addr);
        let m = manifest_with_external("ext-up", &endpoint, "/health");
        let mut s = ExternalDeploy::new(m, serde_json::json!({}), None);
        let prepared = s.prepare().await.expect("probe ok");
        let _ = server.await;
        assert_eq!(prepared.deploy_method, DeployMethod::External);
        assert_eq!(prepared.transport, Transport::ExternalHttp);
        assert_eq!(
            prepared.runtime.endpoint_url.as_deref(),
            Some(endpoint.as_str())
        );
        assert_eq!(prepared.models.len(), 1);
    }
}
