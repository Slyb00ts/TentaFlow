// ============ File: services/auto_detect.rs — best-effort discovery of external daemons at startup ============
//
// Some engines run as user-installed daemons (e.g. Ollama). On boot we look for
// their binary in `PATH` and probe their default endpoint; if both succeed and
// no `services` row already references that engine_id, we register it as an
// `External` service so routing can use it without a manual deploy step.
//
// Failures are silent and non-fatal — auto-detect is convenience, not a
// requirement. Any error is logged at `warn` and the rest of startup proceeds.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::db::DbPool;
use crate::services::deploy::{deploy, DeployError};
use crate::services::manifest::registry;
use crate::services::ports::PortAllocator;
use crate::services_repo::services::{self as services_repo, DeployMethod};

/// Probes for a local Ollama daemon and registers it as an external service
/// if reachable. Returns `Ok(Some(id))` when a new row was inserted,
/// `Ok(None)` when nothing changed (binary missing, daemon down, or already
/// registered), and `Err` only on unexpected DB errors.
pub async fn auto_register_ollama(db: &DbPool, ports: Arc<PortAllocator>) -> Result<Option<i64>> {
    // 1. Manifest must be in the registry — it carries the canonical
    //    detection_endpoint and detection_health_path.
    let reg = registry();
    let manifest = match reg.by_id("ollama") {
        Some(m) => m.clone(),
        None => {
            tracing::debug!("auto_detect: 'ollama' manifest not registered");
            return Ok(None);
        }
    };

    let external = match manifest.deploy.external.as_ref() {
        Some(e) => e,
        None => {
            tracing::debug!("auto_detect: 'ollama' manifest has no [deploy.external]");
            return Ok(None);
        }
    };

    // 2. Binary in PATH? Skip silently when the user does not have ollama installed.
    if which::which(&external.detection_binary).is_err() {
        tracing::debug!(
            "auto_detect: '{}' binary not found in PATH",
            external.detection_binary
        );
        return Ok(None);
    }

    // 3. Daemon reachable?
    let health_url = format!(
        "{}{}",
        external.detection_endpoint.trim_end_matches('/'),
        external.detection_health_path
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let healthy = matches!(
        client.get(&health_url).send().await,
        Ok(resp) if resp.status().is_success()
    );
    if !healthy {
        tracing::debug!("auto_detect: ollama daemon unreachable at {}", health_url);
        return Ok(None);
    }

    // 4. Already registered? `engine_id` plus deploy_method=External is the
    //    natural primary key for an external row — at most one per engine.
    let already = {
        let conn = db
            .lock()
            .map_err(|e| anyhow::anyhow!("services pool lock poisoned: {}", e))?;
        services_repo::list_all(&conn)?
            .into_iter()
            .any(|s| s.engine_id == "ollama" && s.deploy_method == DeployMethod::External)
    };
    if already {
        tracing::info!("auto_detect: ollama already registered, skipping");
        return Ok(None);
    }

    // 5. Persist via the unified deploy pipeline so model_registry rows get
    //    populated identically to a manual external deploy.
    let user_config = serde_json::json!({});
    match deploy(
        DeployMethod::External,
        &manifest,
        &user_config,
        &ports,
        db,
        None,
        None,
    )
    .await
    {
        Ok(outcome) => {
            tracing::info!(
                "auto_detect: registered ollama as service id={} ({})",
                outcome.endpoint.handle.id,
                outcome
                    .endpoint
                    .url
                    .as_deref()
                    .unwrap_or(&external.detection_endpoint)
            );
            Ok(Some(outcome.endpoint.handle.id))
        }
        Err(DeployError::Other(msg)) if msg.contains("unreachable") => {
            // Daemon went down between our probe and the strategy probe — fine.
            tracing::debug!("auto_detect: ollama became unreachable mid-deploy: {}", msg);
            Ok(None)
        }
        Err(e) => Err(anyhow::anyhow!("auto_detect deploy failed: {}", e)),
    }
}
