// ============ File: services/supervisor.rs — health probe + restart loop for services_v2 ============
//
// The supervisor watches every row in `services_v2` whose status is one of
// {running, degraded, starting} and applies a per-transport health probe at a
// configurable cadence. On `Failed`, exponential backoff is applied before
// `services::deploy::respawn` is invoked to bring the runtime back; on
// `Degraded` the row is annotated but no restart is triggered (transient
// upstream blip). Repeated failures past `max_restart_attempts` mark the row
// `failed` permanently.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{watch, Mutex};

use crate::db::DbPool;
use crate::services::deploy::{self, RuntimeHandle};
use crate::services::ports::PortAllocator;
use crate::services::transport::Transport;
use crate::services_repo::services::{self as services_repo, ServiceRow, ServiceStatus};

// ----- Public types ---------------------------------------------------------

/// Aggregate health classification produced by [`check_health`]. Mirrors the
/// three persisted states the DB cares about: ok (green), degraded (yellow),
/// failed (red).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    Ok,
    Degraded(String),
    Failed(String),
}

/// Optional in-process probe consulted for `Embedded` transports. Returning
/// `None` tells the supervisor to treat the engine as healthy by default —
/// the legitimate behaviour while no inference manager is wired up yet
/// (Phase 5 will inject a concrete implementation).
#[async_trait::async_trait]
pub trait EmbeddedHealthProbe: Send + Sync {
    async fn probe(&self, engine_id: &str) -> HealthStatus;
}

/// Phase 5 placeholder: until `LocalInferenceManager` exposes a real probe,
/// embedded engines are reported `Ok` unconditionally. Replace the binding in
/// `main.rs` once the manager grows a `health_for(engine_id)` accessor.
pub struct AlwaysOkEmbeddedProbe;

#[async_trait::async_trait]
impl EmbeddedHealthProbe for AlwaysOkEmbeddedProbe {
    async fn probe(&self, _engine_id: &str) -> HealthStatus {
        HealthStatus::Ok
    }
}

/// Snapshot of the supervised fleet, refreshed once per loop iteration. Wired
/// into the watch channel so consumers (router in Phase 5) can subscribe
/// without polling the DB.
#[derive(Debug, Clone, Default)]
pub struct ServicesSnapshot {
    pub services: Vec<ServiceEntry>,
    pub models_by_name: HashMap<String, i64>,
    pub services_by_id: HashMap<i64, usize>,
    pub generated_at_unix_ms: u128,
}

#[derive(Debug, Clone)]
pub struct ServiceEntry {
    pub id: i64,
    pub engine_id: String,
    pub transport: Transport,
    pub status: ServiceStatus,
    pub endpoint_url: Option<String>,
    pub runtime_port: Option<u16>,
    pub sidecar_quic_port: Option<u16>,
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub id: i64,
    pub model_name: String,
    pub display_name: Option<String>,
    pub is_default: bool,
}

// ----- Errors ---------------------------------------------------------------

#[derive(thiserror::Error, Debug)]
pub enum SupervisorError {
    #[error("db error: {0}")]
    Database(String),
    #[error("config: {0}")]
    Config(String),
}

impl From<anyhow::Error> for SupervisorError {
    fn from(e: anyhow::Error) -> Self {
        SupervisorError::Database(format!("{:#}", e))
    }
}

// ----- Restart bookkeeping --------------------------------------------------

#[derive(Debug, Clone)]
struct RestartState {
    attempts: u32,
    next_backoff: Duration,
    last_attempt: Option<Instant>,
}

impl RestartState {
    fn new(initial_backoff: Duration) -> Self {
        Self {
            attempts: 0,
            next_backoff: initial_backoff,
            last_attempt: None,
        }
    }

    fn ready(&self) -> bool {
        match self.last_attempt {
            None => true,
            Some(t) => t.elapsed() >= self.next_backoff,
        }
    }

    fn record_attempt(&mut self, max_backoff: Duration) {
        self.attempts = self.attempts.saturating_add(1);
        self.last_attempt = Some(Instant::now());
        self.next_backoff = (self.next_backoff.saturating_mul(2)).min(max_backoff);
    }
}

// ----- Supervisor -----------------------------------------------------------

pub struct Supervisor {
    interval: Duration,
    max_restart_attempts: u32,
    restart_backoff_max: Duration,
    initial_backoff: Duration,
    db: DbPool,
    ports: Arc<PortAllocator>,
    snapshot_tx: watch::Sender<Arc<ServicesSnapshot>>,
    restart_state: Arc<Mutex<HashMap<i64, RestartState>>>,
    embedded_probe: Option<Arc<dyn EmbeddedHealthProbe>>,
    health_timeout: Duration,
}

impl Supervisor {
    /// Builds a supervisor from runtime config plus the shared DB / port allocator.
    /// Returns the supervisor itself and a `watch::Receiver` for the snapshot —
    /// the receiver can be passed to consumers that want push notifications when
    /// the fleet changes.
    pub fn new(
        config: &crate::config::ServicesRuntimeConfig,
        db: DbPool,
        ports: Arc<PortAllocator>,
    ) -> (Self, watch::Receiver<Arc<ServicesSnapshot>>) {
        let (tx, rx) = watch::channel(Arc::new(ServicesSnapshot::default()));
        let initial = Duration::from_secs(1);
        let supervisor = Self {
            interval: Duration::from_millis(config.health_check_interval_ms.max(100)),
            max_restart_attempts: config.max_restart_attempts,
            restart_backoff_max: Duration::from_millis(config.restart_backoff_max_ms.max(1_000)),
            initial_backoff: initial,
            db,
            ports,
            snapshot_tx: tx,
            restart_state: Arc::new(Mutex::new(HashMap::new())),
            embedded_probe: None,
            health_timeout: Duration::from_secs(3),
        };
        (supervisor, rx)
    }

    /// Optional: inject the embedded engine probe (Phase 5 hook). Without this
    /// the supervisor treats every embedded service as healthy.
    pub fn with_embedded_probe(mut self, probe: Arc<dyn EmbeddedHealthProbe>) -> Self {
        self.embedded_probe = Some(probe);
        self
    }

    /// Synchronous first tick — runs at startup before the router is online so
    /// the initial snapshot is non-empty. PID liveness is consulted before any
    /// probe; a stale row is marked `failed` (the loop will respawn it).
    pub async fn run_first_tick(&self) -> Result<(), SupervisorError> {
        let services = self.read_supervised().await?;

        for svc in &services {
            // PID-reuse defence runs only for transports that actually own a process.
            if let Some(pid) = svc.runtime_pid {
                let needs_pid_check = matches!(
                    svc.transport,
                    Transport::HttpDirect | Transport::SidecarQuic
                );
                if needs_pid_check
                    && !crate::services::lifecycle::pid_alive_with_cmdline_marker(
                        pid as i32,
                        &svc.engine_id,
                    )
                {
                    let msg = format!("pid {} no longer alive at startup", pid);
                    self.mark_health(svc.id, false, Some(&msg)).await;
                    self.mark_status(svc.id, ServiceStatus::Failed, Some(&msg))
                        .await;
                    continue;
                }
            }

            let health = self
                .check_health(svc.transport, svc.endpoint_url.as_deref(), svc.runtime_port)
                .await;
            self.apply_health(svc, health, /*allow_restart=*/ false)
                .await;
        }

        let snapshot = self.build_snapshot().await?;
        let _ = self.snapshot_tx.send(Arc::new(snapshot));
        Ok(())
    }

    /// Spawns the detached supervisor loop. The returned handle joins to `()`
    /// when the channel is dropped; in production we leak it intentionally.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run_loop().await;
        })
    }

    async fn run_loop(self) {
        loop {
            tokio::time::sleep(self.interval).await;

            let services = match self.read_supervised().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("supervisor: list_supervised failed: {}", e);
                    continue;
                }
            };

            for svc in &services {
                let health = self
                    .check_health(svc.transport, svc.endpoint_url.as_deref(), svc.runtime_port)
                    .await;
                self.apply_health(svc, health, /*allow_restart=*/ true)
                    .await;
            }

            match self.build_snapshot().await {
                Ok(snap) => {
                    let _ = self.snapshot_tx.send(Arc::new(snap));
                }
                Err(e) => tracing::warn!("supervisor: build_snapshot failed: {}", e),
            }
        }
    }

    // ---- DB I/O ------------------------------------------------------------

    async fn read_supervised(&self) -> Result<Vec<ServiceRow>, SupervisorError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db
                .lock()
                .map_err(|e| SupervisorError::Database(format!("pool poisoned: {}", e)))?;
            services_repo::list_supervised(&conn)
                .map_err(|e| SupervisorError::Database(e.to_string()))
        })
        .await
        .map_err(|e| SupervisorError::Database(format!("join: {}", e)))?
    }

    async fn mark_status(&self, id: i64, status: ServiceStatus, err: Option<&str>) {
        let db = self.db.clone();
        let err_owned = err.map(|s| s.to_string());
        let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = db
                .lock()
                .map_err(|e| anyhow::anyhow!("pool poisoned: {}", e))?;
            services_repo::update_status(&conn, id, status)?;
            if let Some(msg) = err_owned {
                services_repo::update_health(&conn, id, false, Some(&msg))?;
            }
            Ok(())
        })
        .await;
    }

    async fn mark_health(&self, id: i64, ok: bool, err: Option<&str>) {
        let db = self.db.clone();
        let err_owned = err.map(|s| s.to_string());
        let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = db
                .lock()
                .map_err(|e| anyhow::anyhow!("pool poisoned: {}", e))?;
            services_repo::update_health(&conn, id, ok, err_owned.as_deref())?;
            Ok(())
        })
        .await;
    }

    async fn write_runtime(&self, id: i64, runtime: &RuntimeHandle) -> Result<(), SupervisorError> {
        let db = self.db.clone();
        let pid = runtime.pid;
        let port = runtime.port;
        let sidecar = runtime.sidecar_port;
        let url = runtime.endpoint_url.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = db
                .lock()
                .map_err(|e| anyhow::anyhow!("pool poisoned: {}", e))?;
            services_repo::update_runtime(&conn, id, pid, port, sidecar, url.as_deref())?;
            Ok(())
        })
        .await
        .map_err(|e| SupervisorError::Database(format!("join: {}", e)))?
        .map_err(|e| SupervisorError::Database(e.to_string()))
    }

    async fn increment_restart(&self, id: i64) {
        let db = self.db.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(conn) = db.lock() {
                let _ = services_repo::increment_restart(&conn, id);
            }
        })
        .await;
    }

    async fn build_snapshot(&self) -> Result<ServicesSnapshot, SupervisorError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<ServicesSnapshot, SupervisorError> {
            let conn = db
                .lock()
                .map_err(|e| SupervisorError::Database(format!("pool poisoned: {}", e)))?;
            let rows = services_repo::list_supervised(&conn)
                .map_err(|e| SupervisorError::Database(e.to_string()))?;

            let mut services = Vec::with_capacity(rows.len());
            let mut models_by_name = HashMap::new();
            let mut services_by_id = HashMap::new();

            for row in rows {
                if !matches!(row.status, ServiceStatus::Running | ServiceStatus::Degraded) {
                    continue;
                }
                let models = crate::services_repo::models::list_for_service(&conn, row.id)
                    .map_err(|e| SupervisorError::Database(e.to_string()))?;
                let model_entries: Vec<ModelEntry> = models
                    .into_iter()
                    .map(|m| {
                        models_by_name.insert(m.model_name.clone(), row.id);
                        ModelEntry {
                            id: m.id,
                            model_name: m.model_name,
                            display_name: m.display_name,
                            is_default: m.is_default,
                        }
                    })
                    .collect();
                let idx = services.len();
                services_by_id.insert(row.id, idx);
                services.push(ServiceEntry {
                    id: row.id,
                    engine_id: row.engine_id,
                    transport: row.transport,
                    status: row.status,
                    endpoint_url: row.endpoint_url,
                    runtime_port: row.runtime_port,
                    sidecar_quic_port: row.sidecar_quic_port,
                    models: model_entries,
                });
            }

            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);

            Ok(ServicesSnapshot {
                services,
                models_by_name,
                services_by_id,
                generated_at_unix_ms: now_ms,
            })
        })
        .await
        .map_err(|e| SupervisorError::Database(format!("join: {}", e)))?
    }

    // ---- Health-check dispatch --------------------------------------------

    async fn check_health(
        &self,
        transport: Transport,
        endpoint_url: Option<&str>,
        runtime_port: Option<u16>,
    ) -> HealthStatus {
        match transport {
            Transport::Embedded => match &self.embedded_probe {
                Some(p) => p.probe("").await,
                None => HealthStatus::Ok,
            },
            Transport::HttpDirect => match endpoint_url {
                Some(url) => http_probe(url, self.health_timeout).await,
                None => HealthStatus::Failed("HttpDirect: missing endpoint_url".into()),
            },
            Transport::ExternalHttp => match endpoint_url {
                Some(url) => http_probe(url, self.health_timeout).await,
                None => HealthStatus::Failed("ExternalHttp: missing endpoint_url".into()),
            },
            Transport::SidecarQuic => {
                // QUIC bidi ping infrastructure is not yet exposed for the
                // supervisor; fall back to the runtime HTTP port the sidecar
                // also exposes. When QUIC ping support lands the branch will
                // pick it up first and treat HTTP as the secondary probe.
                let port = match runtime_port {
                    Some(p) => p,
                    None => {
                        return HealthStatus::Failed(
                            "SidecarQuic: missing runtime_port for HTTP fallback".into(),
                        )
                    }
                };
                let url = format!("http://127.0.0.1:{}/v1/models", port);
                http_probe(&url, self.health_timeout).await
            }
        }
    }

    // ---- Reaction logic ----------------------------------------------------

    async fn apply_health(&self, svc: &ServiceRow, health: HealthStatus, allow_restart: bool) {
        match health {
            HealthStatus::Ok => {
                self.mark_health(svc.id, true, None).await;
                if svc.status != ServiceStatus::Running {
                    self.mark_status(svc.id, ServiceStatus::Running, None).await;
                }
                self.clear_restart_state(svc.id).await;
            }
            HealthStatus::Degraded(reason) => {
                self.mark_health(svc.id, false, Some(&reason)).await;
                if svc.status != ServiceStatus::Degraded {
                    self.mark_status(svc.id, ServiceStatus::Degraded, Some(&reason))
                        .await;
                }
            }
            HealthStatus::Failed(reason) => {
                self.mark_health(svc.id, false, Some(&reason)).await;
                if !allow_restart {
                    return;
                }
                let mut states = self.restart_state.lock().await;
                let state = states
                    .entry(svc.id)
                    .or_insert_with(|| RestartState::new(self.initial_backoff));

                if state.attempts >= self.max_restart_attempts {
                    let msg = format!("permanent failure after {} attempts", state.attempts);
                    drop(states);
                    self.mark_status(svc.id, ServiceStatus::Failed, Some(&msg))
                        .await;
                    return;
                }
                if !state.ready() {
                    return;
                }

                state.record_attempt(self.restart_backoff_max);
                let attempt = state.attempts;
                drop(states);

                self.mark_status(svc.id, ServiceStatus::Starting, None)
                    .await;
                self.increment_restart(svc.id).await;

                match deploy::respawn(
                    &svc.engine_id,
                    svc.deploy_method,
                    &svc.config_json,
                    self.ports.clone(),
                )
                .await
                {
                    Ok(handle) => {
                        if let Err(e) = self.write_runtime(svc.id, &handle).await {
                            tracing::warn!(
                                "supervisor: write_runtime failed for {}: {}",
                                svc.id,
                                e
                            );
                        }
                        self.mark_status(svc.id, ServiceStatus::Running, None).await;
                        self.clear_restart_state(svc.id).await;
                        tracing::info!(
                            "supervisor: respawn ok for service {} ({})",
                            svc.id,
                            svc.engine_id
                        );
                    }
                    Err(e) => {
                        let msg = format!("restart {}: {}", attempt, e);
                        self.mark_status(svc.id, ServiceStatus::Failed, Some(&msg))
                            .await;
                        tracing::warn!(
                            "supervisor: respawn failed for service {} ({}): {}",
                            svc.id,
                            svc.engine_id,
                            e
                        );
                    }
                }
            }
        }
    }

    async fn clear_restart_state(&self, id: i64) {
        let mut g = self.restart_state.lock().await;
        g.remove(&id);
    }
}

// ----- HTTP probe helper ----------------------------------------------------

async fn http_probe(url: &str, timeout: Duration) -> HealthStatus {
    // Engines speak OpenAI-compatible APIs in the dominant case, so probe the
    // /v1/models endpoint when no explicit health URL is provided.
    let probe_url = if url.ends_with('/') || url.contains("/v1/") || url.contains("/health") {
        url.to_string()
    } else {
        format!("{}/v1/models", url.trim_end_matches('/'))
    };

    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(e) => return HealthStatus::Failed(format!("reqwest builder: {}", e)),
    };

    match client.get(&probe_url).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                HealthStatus::Ok
            } else if status.as_u16() == 404 && !probe_url.ends_with("/health") {
                // Fallback: try /health if /v1/models is not exposed.
                let base = url.trim_end_matches('/').trim_end_matches("/v1/models");
                let fallback = format!("{}/health", base);
                match client.get(&fallback).send().await {
                    Ok(r2) if r2.status().is_success() => HealthStatus::Ok,
                    Ok(r2) => HealthStatus::Degraded(format!("http {}", r2.status())),
                    Err(e) => HealthStatus::Failed(format!("http error: {}", e)),
                }
            } else {
                HealthStatus::Degraded(format!("http {}", status))
            }
        }
        Err(e) if e.is_timeout() => HealthStatus::Failed(format!("timeout: {}", e)),
        Err(e) if e.is_connect() => HealthStatus::Failed(format!("connect: {}", e)),
        Err(e) => HealthStatus::Failed(format!("http error: {}", e)),
    }
}

// ----- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services_repo::services::{DeployMethod, NewService};
    use std::sync::{Arc, Mutex as StdMutex};

    fn open_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        Arc::new(StdMutex::new(conn))
    }

    fn ports_for_test(lo: u16, hi: u16) -> Arc<PortAllocator> {
        Arc::new(PortAllocator::new((lo, hi), Default::default()).unwrap())
    }

    fn cfg(
        interval_ms: u64,
        max_restart: u32,
        backoff_max_ms: u64,
    ) -> crate::config::ServicesRuntimeConfig {
        crate::config::ServicesRuntimeConfig {
            port_range: (50_000, 50_100),
            health_check_interval_ms: interval_ms,
            max_restart_attempts: max_restart,
            restart_backoff_max_ms: backoff_max_ms,
        }
    }

    #[cfg(feature = "dashboard-api")]
    #[tokio::test]
    async fn http_probe_ok_on_200() {
        // Spin up a tiny axum server returning 200 on /v1/models.
        let app = axum::Router::new().route(
            "/v1/models",
            axum::routing::get(|| async { axum::http::StatusCode::OK }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let url = format!("http://{}", addr);
        let h = http_probe(&url, Duration::from_secs(2)).await;
        assert_eq!(h, HealthStatus::Ok);
    }

    #[tokio::test]
    async fn http_probe_failed_on_unreachable() {
        // Bind a port then drop the listener to ensure the address is closed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let url = format!("http://{}", addr);
        let h = http_probe(&url, Duration::from_millis(500)).await;
        assert!(matches!(h, HealthStatus::Failed(_)), "got {:?}", h);
    }

    #[test]
    fn restart_state_backoff_doubles() {
        let mut s = RestartState::new(Duration::from_secs(1));
        let cap = Duration::from_secs(60);
        s.record_attempt(cap);
        assert_eq!(s.next_backoff, Duration::from_secs(2));
        s.record_attempt(cap);
        assert_eq!(s.next_backoff, Duration::from_secs(4));
        s.record_attempt(cap);
        assert_eq!(s.next_backoff, Duration::from_secs(8));
        assert_eq!(s.attempts, 3);
    }

    #[test]
    fn restart_state_caps_at_max() {
        let mut s = RestartState::new(Duration::from_secs(1));
        let cap = Duration::from_secs(60);
        for _ in 0..20 {
            s.record_attempt(cap);
        }
        assert_eq!(s.next_backoff, cap);
    }

    #[tokio::test]
    async fn permanent_failure_after_max_attempts() {
        let db = open_db();
        let ports = ports_for_test(50_500, 50_510);
        let conf = cfg(1_000, 2, 60_000);
        let (sup, _rx) = Supervisor::new(&conf, db.clone(), ports);

        // Insert a row that will never come back: an http_direct entry pointed
        // at a closed port, so respawn() will also fail because the manifest
        // is not in the global registry.
        let id = {
            let conn = db.lock().unwrap();
            services_repo::insert(
                &conn,
                &NewService {
                    engine_id: "supervisor-bogus".into(),
                    deploy_method: DeployMethod::Docker,
                    transport: Transport::HttpDirect,
                    status: ServiceStatus::Running,
                    runtime_pid: None,
                    runtime_port: Some(1),
                    sidecar_quic_port: None,
                    endpoint_url: Some("http://127.0.0.1:1".into()),
                    config_json: "{}".into(),
                },
            )
            .unwrap()
        };

        // Drive the supervisor manually: read the row, fail it twice, third
        // time it must flip to permanently `failed` and stop trying.
        for _ in 0..3 {
            let svc = {
                let conn = db.lock().unwrap();
                services_repo::get(&conn, id).unwrap().unwrap()
            };
            sup.apply_health(&svc, HealthStatus::Failed("test".into()), true)
                .await;
            // Bypass backoff timing: force-reset last_attempt so the next call
            // proceeds immediately.
            {
                let mut g = sup.restart_state.lock().await;
                if let Some(s) = g.get_mut(&id) {
                    s.last_attempt = Some(Instant::now() - Duration::from_secs(3_600));
                }
            }
        }

        let final_status = {
            let conn = db.lock().unwrap();
            services_repo::get(&conn, id).unwrap().unwrap().status
        };
        assert_eq!(final_status, ServiceStatus::Failed);
    }

    #[tokio::test]
    async fn snapshot_includes_alive_only() {
        let db = open_db();
        let ports = ports_for_test(50_600, 50_610);
        let conf = cfg(1_000, 5, 60_000);
        let (sup, _rx) = Supervisor::new(&conf, db.clone(), ports);

        let (alive, stopped) = {
            let conn = db.lock().unwrap();
            let a = services_repo::insert(
                &conn,
                &NewService {
                    engine_id: "alive".into(),
                    deploy_method: DeployMethod::NativeEmbedded,
                    transport: Transport::Embedded,
                    status: ServiceStatus::Running,
                    runtime_pid: None,
                    runtime_port: None,
                    sidecar_quic_port: None,
                    endpoint_url: None,
                    config_json: "{}".into(),
                },
            )
            .unwrap();
            let s = services_repo::insert(
                &conn,
                &NewService::minimal("stopped", DeployMethod::NativeEmbedded, Transport::Embedded),
            )
            .unwrap();
            services_repo::update_status(&conn, s, ServiceStatus::Stopped).unwrap();
            (a, s)
        };

        let snap = sup.build_snapshot().await.unwrap();
        let ids: Vec<i64> = snap.services.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![alive]);
        assert!(!ids.contains(&stopped));
    }

    #[tokio::test]
    async fn clear_restart_state_on_recovery() {
        let db = open_db();
        let ports = ports_for_test(50_700, 50_710);
        let conf = cfg(1_000, 5, 60_000);
        let (sup, _rx) = Supervisor::new(&conf, db.clone(), ports);

        let id = {
            let conn = db.lock().unwrap();
            services_repo::insert(
                &conn,
                &NewService {
                    engine_id: "recover".into(),
                    deploy_method: DeployMethod::NativeEmbedded,
                    transport: Transport::Embedded,
                    status: ServiceStatus::Running,
                    runtime_pid: None,
                    runtime_port: None,
                    sidecar_quic_port: None,
                    endpoint_url: None,
                    config_json: "{}".into(),
                },
            )
            .unwrap()
        };

        let svc = {
            let conn = db.lock().unwrap();
            services_repo::get(&conn, id).unwrap().unwrap()
        };
        sup.apply_health(&svc, HealthStatus::Failed("blip".into()), false)
            .await;
        // Force-insert restart state to verify recovery clears it.
        {
            let mut g = sup.restart_state.lock().await;
            g.insert(id, RestartState::new(Duration::from_secs(1)));
        }
        sup.apply_health(&svc, HealthStatus::Ok, true).await;
        let g = sup.restart_state.lock().await;
        assert!(!g.contains_key(&id), "state must be cleared after recovery");
    }
}
