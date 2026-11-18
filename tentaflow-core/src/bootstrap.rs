// =============================================================================
// File: bootstrap.rs
// Purpose: Node startup shared by the desktop binary (`tentaflow`) and the
//          mobile runtime (`tentaflow-mobile`). Each phase is one function so a
//          host can slot its platform-only steps (Docker, GPU workers, the
//          app sandbox, the iOS memory guard) between them without keeping a
//          second copy of everything else.
// Example:
//          let db = bootstrap::open_database(&config, &db_path)?;
//          let identity = bootstrap::init_core_state(&db, None, paths::data_dir())?;
//          let mut node = bootstrap::build_router(&config, &db, identity).await?;
//          let addons = node.start_router_and_addons()?;
//          let mesh = node.start_mesh(&addons, "router", None).await;
//          let metrics = node.start_background_services(mesh.as_ref()).await;
//          node.start_server(&metrics, &addons, mesh.as_ref())?;
//          // ... until the host is asked to stop:
//          node.stop_process_runtime().await?;
//          node.shutdown(&addons, mesh, None).await;
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{error, info, warn};

use crate::addon::AddonManager;
use crate::config::NodeConfig;
use crate::crypto::SettingsCipher;
use crate::db::{self, DbPool};
use crate::mesh::iroh_manager::IrohMeshManager;
use crate::mesh::peer_store::MeshPeerStore;
use crate::mesh::pipeline::{start_mesh_pipeline, MeshPipelineConfig, MeshPipelineHandles};
use crate::mesh::security::MeshSecurity;
use crate::metrics::{collector::MetricsCollector, RouterMetrics};
use crate::paths;
use crate::routing::Router;
use crate::services::mesh_registry::MeshServicesRegistry;
use crate::services::ports::PortAllocator;

/// Node identity and the cipher every encrypted setting goes through.
pub struct CoreIdentity {
    pub settings_cipher: Arc<SettingsCipher>,
    pub mesh_security: Arc<MeshSecurity>,
    /// Ed25519 public key hex — the node id everywhere (mesh, sync, GUI).
    pub local_node_id: String,
}

/// Everything a running node is made of once the router exists.
pub struct CoreNode {
    pub config: NodeConfig,
    pub db: DbPool,
    pub settings_cipher: Arc<SettingsCipher>,
    pub mesh_security: Arc<MeshSecurity>,
    pub local_node_id: String,
    pub peer_store: MeshPeerStore,
    pub mesh_services_registry: Arc<MeshServicesRegistry>,
    /// `None` when `[services_runtime].port_range` is invalid: the supervisor
    /// is then disabled as a whole.
    pub port_allocator: Option<Arc<PortAllocator>>,
    pub router: Arc<Router>,
    /// Set on graceful shutdown before services are stopped, so the health
    /// loop does not respawn engines that are being torn down.
    pub supervisor_shutdown: Option<Arc<AtomicBool>>,
    /// Set by `start_background_services`; awaited by `shutdown` before the
    /// WAL checkpoint.
    sla_evaluator: Option<tokio::task::JoinHandle<()>>,
}

/// Freezes the process-wide settings taken from the config, opens the core
/// database and starts the write-behind workers that flush into it.
pub fn open_database(config: &NodeConfig, db_path: &Path) -> Result<DbPool> {
    // The vision settings are frozen before anything vision-related (camera
    // ingest, detector pools, worker supervisor) can read them — the config
    // TOML is the only operator mechanism.
    crate::vision::settings::init(config.vision.clone()).map_err(|e| {
        error!("Vision settings init: {}", e);
        anyhow::anyhow!("vision settings init: {}", e)
    })?;

    crate::compliance::ai_gateway::set_token_quota_enabled(config.token_metrics.enabled);

    // Async by default: the write leaves the request hot path (~2 ms/request,
    // no serialisation on the SQLite writer mutex). `TENTAFLOW_AI_AUDIT_SYNC=1`
    // is the compliance-strict mode: the prompt is persisted BEFORE dispatch
    // and survives a crash.
    let audit_sync = std::env::var("TENTAFLOW_AI_AUDIT_SYNC")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    crate::compliance::audit_worker::set_audit_async(!audit_sync);
    crate::compliance::audit_worker::init_audit_worker();
    info!(
        "AI audit mode: {}",
        if audit_sync { "sync" } else { "async" }
    );

    info!("Inicjalizacja bazy danych: {:?}", db_path);
    let db = db::init(db_path).map_err(|e| {
        error!("Blad inicjalizacji bazy danych: {}", e);
        e
    })?;
    // Metric rollups and usage counters leave the hot path: the worker batches
    // increments into one transaction every ~200 ms / 512 jobs, and quota
    // enforcement reads the unflushed part from memory. Both need the pool, and
    // the cache goes first because the worker picks it up on its first flush.
    crate::services::runtime::token_usage_cache::init_token_usage_cache(db.clone());
    crate::services::runtime::metrics_worker::init_metrics_worker(db.clone());
    Ok(db)
}

/// Opens the dedicated stores, loads the master key and the node identity,
/// and brings the Sync Ledger up. `master_key_dir` = `None` uses the default
/// key location; a sandboxed host passes its own data directory. Addon
/// packages are stored under `packages_base`.
pub fn init_core_state(
    db: &DbPool,
    master_key_dir: Option<&Path>,
    packages_base: PathBuf,
) -> Result<CoreIdentity> {
    // Before the ledger: applying a replicated addon package from the inbox
    // writes it under the packages root.
    crate::addon::bundled::set_packages_base(packages_base);
    // ML Studio has its own SQLite file, pool and migration runner.
    crate::ml_studio::init(paths::tentaflow_home()).map_err(|e| {
        error!("Blad inicjalizacji bazy ML Studio: {}", e);
        e
    })?;
    // Project Studio: central registry in `data/projects.db` plus per-project
    // pools with an idle sweeper.
    crate::project_studio::init(db).map_err(|e| {
        error!("Blad inicjalizacji bazy Project Studio: {}", e);
        e
    })?;
    // The ingest queue has its own writer (a claim/heartbeat/finish would
    // otherwise serialise behind every write on the main database) and a file
    // separate from `events.db`, whose retention must never drop queued work.
    // Reconciliation runs BEFORE any worker can claim: a job left `running` by
    // a process that no longer exists is closed, a `queued` one is kept.
    crate::services::ingest_jobs::init(&paths::data_dir().join("jobs.db")).map_err(|e| {
        error!("Ingest queue initialisation failed: {}", e);
        e
    })?;
    crate::project_studio::ingest::reconcile_orphans();

    // The event log has its own writer so a high-frequency timeline never
    // queues behind the main database. This also starts the audit outbox
    // delivery and the retention sweep, without which the file only grows.
    crate::events::init(db).map_err(|e| {
        error!("Event log initialisation failed: {}", e);
        e
    })?;
    match db::repository::ensure_default_core_sync_policies(db) {
        Ok(n) if n > 0 => info!("Sync Ledger zasiał {} domyślnych polityk core", n),
        Err(e) => error!("Sync Ledger nie zasiał domyślnych polityk core: {}", e),
        _ => {}
    }
    crate::bus::schema_registry::registry::warn_about_stored_hidden_characters(db);
    match db::repository::ensure_trusted_nodes_in_sync_identity(db) {
        Ok(n) if n > 0 => info!("Sync Ledger zarejestrował {} zaufanych nodów mesh", n),
        Err(e) => error!("Sync Ledger nie zarejestrował zaufanych nodów mesh: {}", e),
        _ => {}
    }
    // A revocation, or the node being taken out of an account fleet, can land
    // while the node is down; nothing else looks at the credentials stored on
    // this disk until somebody uses the account.
    crate::provider_accounts::credential_sync::spawn_reconcile(db);
    // Without the schedule an addon ingest queue (`ingest_drain` tool) stands
    // still until someone creates a job by hand. Idempotent per restart.
    match crate::scheduler::ensure_addon_ingest_drain_schedules(db) {
        Ok(n) if n > 0 => info!("Scheduler zapewnił {} auto-jobów drainu ingestu", n),
        Err(e) => error!("Scheduler nie zapewnił auto-jobów drainu ingestu: {}", e),
        _ => {}
    }

    // Robots keep their real-world georeference across restarts.
    crate::dispatch::robots::load_geo_anchors(db);

    // The legacy UUID node id was replaced by the iroh EndpointId.
    let _ = db::repository::delete_setting(db, "node_id");

    let file_master_key = crate::crypto::load_or_create_master_key_in(master_key_dir)
        .context("Nie udalo sie zaladowac master key z pliku")?;
    let settings_cipher = Arc::new(SettingsCipher::new(&file_master_key));

    match crate::crypto::migrate_plaintext_secrets(db, &settings_cipher) {
        Ok(n) if n > 0 => info!("Zaszyfrowano {} plaintextowych sekretow w bazie", n),
        Err(e) => error!("Blad migracji sekretow: {}", e),
        _ => {}
    }

    // The Ed25519 keypair (stored encrypted in settings) is the iroh
    // EndpointId and the node id used by the dashboard and peer routing.
    let mesh_security = Arc::new(
        MeshSecurity::new(db.clone(), settings_cipher.clone()).map_err(|e| {
            error!("MeshSecurity init: {}", e);
            e
        })?,
    );
    let local_node_id = mesh_security.ed25519_public_key_hex();
    info!(
        "Mesh identity: {}",
        &local_node_id[..16.min(local_node_id.len())]
    );
    init_sync_ledger(db, &mesh_security);

    Ok(CoreIdentity {
        settings_cipher,
        mesh_security,
        local_node_id,
    })
}

/// Opens the ledger and replays every capture and inbox row a previous run
/// left pending. A failure is logged, not fatal: the node runs without sync.
fn init_sync_ledger(db: &DbPool, mesh_security: &Arc<MeshSecurity>) {
    if let Err(e) = crate::sync::runtime::init(db.clone(), mesh_security.clone()) {
        error!("Sync Ledger runtime init failed: {}", e);
        return;
    }
    info!("Sync Ledger runtime initialized");
    match crate::sync::runtime::run_pending_baseline_cutover() {
        Ok(Some(reseeded)) => info!(
            "Sync Ledger core baseline reset after v53 cutover: re-seeded {} core ops under new epoch",
            reseeded
        ),
        Ok(None) => {}
        Err(e) => error!("Sync Ledger baseline cutover failed: {}", e),
    }
    match crate::sync::runtime::run_pending_env_partition_remap() {
        Ok(Some(reseeded)) => info!(
            "Sync Ledger core partitions remapped to the environment-scoped namespace after v145: re-seeded {} core ops",
            reseeded
        ),
        Ok(None) => {}
        Err(e) => error!("Sync Ledger environment partition remap failed: {}", e),
    }
    match crate::sync::runtime::run_pending_environment_change_resume() {
        Ok(Some(reseeded)) => info!(
            "Sync Ledger resumed an interrupted environment change (SetKind): re-seeded {} core ops",
            reseeded
        ),
        Ok(None) => {}
        Err(e) => error!("Sync Ledger environment change resume failed: {}", e),
    }
    match crate::addon::storage_sql_exec::drain_installed_sql_captures(db, 1000) {
        Ok(drained) => info!("Sync Ledger drained {} pending SQL captures", drained),
        Err(e) => error!("Sync Ledger SQL capture drain failed: {}", e),
    }
    match db::repository::backfill_model_alias_captures(db) {
        Ok(captured) if captured > 0 => info!(
            "Sync Ledger captured {} model aliases that predate the ledger",
            captured
        ),
        Ok(_) => {}
        Err(e) => error!("Sync Ledger model alias backfill failed: {}", e),
    }
    match crate::sync::core_capture::drain_pending_core_captures(db, 1000) {
        Ok(drained) => info!("Sync Ledger drained {} pending core captures", drained),
        Err(e) => error!("Sync Ledger core capture drain failed: {}", e),
    }
    match crate::sync::kv_capture::drain_pending_kv_captures(db, 1000) {
        Ok(drained) => info!("Sync Ledger drained {} pending KV captures", drained),
        Err(e) => error!("Sync Ledger KV capture drain failed: {}", e),
    }
    match crate::sync::blob_capture::drain_pending_blob_captures(db, 1000) {
        Ok(drained) => info!("Sync Ledger drained {} pending blob captures", drained),
        Err(e) => error!("Sync Ledger blob capture drain failed: {}", e),
    }
    match crate::sync::runtime::apply_unapplied_inbox(1000) {
        Ok(Some(applied)) => info!("Sync Ledger applied {} inbox operations", applied),
        Ok(None) => {}
        Err(e) => error!("Sync Ledger inbox apply failed: {}", e),
    }
}

/// Peer store hydrated from `peer_persisted` / `peer_hints`, with the local
/// node seeded so the catalog, services and mesh GUI always have a "local"
/// target, mesh or no mesh.
fn build_peer_store(db: &DbPool, identity: &CoreIdentity) -> MeshPeerStore {
    let mut peer_store = MeshPeerStore::new();
    let peer_registry = crate::mesh::peer_registry::PeerRegistry::new(4096);
    peer_store.set_registry(peer_registry.clone());

    // The db::init migration copies legacy trusted_nodes and trusted_contact
    // rows into these tables, so this alone restores trust state, hostname,
    // platform and transport hints for every previously paired peer.
    match peer_registry.hydrate_from_db(db) {
        Ok(n) => info!("PeerRegistry hydrated {} peers from peer_persisted", n),
        Err(e) => warn!("PeerRegistry hydrate failed: {}", e),
    }

    // Installed AFTER hydrate so hydration itself does not re-emit writes.
    {
        use crate::mesh::peer_registry::persistence::{
            DbSink, PersistenceWriter, CHANNEL_CAPACITY,
        };
        let sink = Arc::new(DbSink::new(db.clone()));
        let (writer, persist_tx) = PersistenceWriter::new(sink, CHANNEL_CAPACITY);
        peer_registry.set_persistence(persist_tx);
        let _writer_handle = writer.spawn();
    }

    for (node_id, public_key_hex, _approved_at) in identity.mesh_security.get_all_trusted_keys() {
        if node_id != identity.local_node_id {
            peer_store.ensure_trusted_peer(&node_id, &public_key_hex, "");
        }
    }

    {
        use crate::mesh::node_info_collector;
        let local_node_id = &identity.local_node_id;
        let info = node_info_collector::collect_node_info(local_node_id);
        let hostname = info.hostname.clone();
        let platform = node_info_collector::detect_platform();
        let os_info = node_info_collector::collect_os_distro();
        let docker_version = node_info_collector::docker_server_version();
        let addresses = node_info_collector::collect_local_addresses();
        peer_store.seed_local(
            local_node_id,
            hostname,
            if os_info.is_empty() {
                info.os_info.clone()
            } else {
                os_info
            },
            platform,
            info.cpu_count,
            info.ram_total_mb,
            info.gpu_info.clone(),
            addresses,
            docker_version,
        );
        info!(node_id = %local_node_id, "Local node seeded in peer_store");
    }
    peer_store
}

/// Port allocator for the services supervisor, with every port already
/// recorded in the database leased up front.
fn build_port_allocator(config: &NodeConfig, db: &DbPool) -> Option<Arc<PortAllocator>> {
    let port_range = config.services_runtime.port_range;
    // The excluded set stays empty: ports of existing services are LEASED
    // below (released on stop/delete), whereas an excluded port is permanent
    // and would block its own owner on respawn.
    let port_allocator = match PortAllocator::new(port_range, std::collections::HashSet::new()) {
        Ok(allocator) => Arc::new(allocator),
        Err(e) => {
            warn!(
                "Services supervisor disabled: invalid port_range {:?}: {}",
                port_range, e
            );
            return None;
        }
    };

    // After a restart the allocator knows nothing about ports held by existing
    // services; without the lease a parallel deploy could take one and the
    // pinned respawn would silently move to another port.
    match db.read() {
        Ok(conn) => match crate::services_repo::services::list_all(&conn) {
            Ok(services) => {
                for svc in services {
                    for port in [svc.runtime_port, svc.sidecar_quic_port]
                        .into_iter()
                        .flatten()
                    {
                        if let Err(e) = port_allocator.reserve(port) {
                            warn!(
                                service_id = svc.id,
                                port, "boot port reserve skipped: {}", e
                            );
                        }
                    }
                }
            }
            Err(e) => warn!("boot port reserve: list_all failed: {}", e),
        },
        Err(e) => warn!("boot port reserve: db lock poisoned: {}", e),
    }

    // Cluster deployments: serve port + dist_port.
    if let Ok(cdeployments) = db::repository::cluster_deployments_by_status(
        db,
        &["deploying", "running", "degraded", "starting", "stopped"],
    ) {
        for d in cdeployments {
            let _ = port_allocator.reserve(d.port as u16);
            if d.dist_port > 0 {
                let _ = port_allocator.reserve(d.dist_port as u16);
            }
        }
    }
    Some(port_allocator)
}

/// Builds the peer store, the router, the project ingest workers and the
/// services supervisor. The router is NOT started yet: a host may plant its
/// own routing state first; `start_router_and_addons` starts it.
pub async fn build_router(
    config: &NodeConfig,
    db: &DbPool,
    identity: CoreIdentity,
) -> Result<CoreNode> {
    let peer_store = build_peer_store(db, &identity);
    let mesh_services_registry = Arc::new(MeshServicesRegistry::new());
    let port_allocator = build_port_allocator(config, db);

    info!("Inicjalizacja routera...");
    let router = Arc::new(Router::new(config.clone(), Some(db.clone()))?);

    // A job recovered after a restart has no request context to take the
    // router and the pool from, so the workers get them here.
    crate::project_studio::ingest::start_workers(db.clone(), router.clone());

    // The supervisor shares the router's `LiveHandlesCache`, so reconcile()
    // updates the cache the routing call sites read: router first, supervisor
    // second, and the snapshot receiver goes in BEFORE `router.start()`.
    let mut supervisor_shutdown = None;
    if let Some(port_allocator) = port_allocator.clone() {
        use crate::services::supervisor::{DefaultEmbeddedProbe, Supervisor};
        let services_runtime_cfg = &config.services_runtime;
        let live_handles = router.service_manager().live_handles.clone();
        let (supervisor, snapshot_rx) = Supervisor::new(
            services_runtime_cfg,
            db.clone(),
            port_allocator,
            identity.settings_cipher.clone(),
            identity.local_node_id.clone(),
            mesh_services_registry.clone(),
            live_handles,
        );
        let supervisor = supervisor
            .with_embedded_probe(Arc::new(DefaultEmbeddedProbe))
            .with_catalog_provider(router.catalog_provider().clone());

        // Synchronous first tick: the initial snapshot is non-empty before the
        // router goes online.
        if let Err(e) = supervisor.run_first_tick().await {
            warn!("services supervisor: first_tick failed: {}", e);
        }

        // Taken before spawn() consumes the supervisor.
        supervisor_shutdown = Some(supervisor.shutdown_flag());
        let _supervisor_handle = supervisor.spawn();
        info!(
            "Services supervisor started (interval={}ms, port_range={:?})",
            services_runtime_cfg.health_check_interval_ms, services_runtime_cfg.port_range
        );
        router.set_services_snapshot_rx(snapshot_rx);
    }

    Ok(CoreNode {
        config: config.clone(),
        db: db.clone(),
        settings_cipher: identity.settings_cipher,
        mesh_security: identity.mesh_security,
        local_node_id: identity.local_node_id,
        peer_store,
        mesh_services_registry,
        port_allocator,
        router,
        supervisor_shutdown,
        sla_evaluator: None,
    })
}

impl CoreNode {
    /// Starts the router, installs the bundled addons and native app packages,
    /// and brings up the addon manager with everything that hangs off it:
    /// tools, native app instances, event bus, flow resolvers, agent runs and
    /// service-mode addons.
    pub fn start_router_and_addons(&self) -> Result<Arc<AddonManager>> {
        let db = &self.db;
        let router = &self.router;

        // The routing path resolves handles across local and remote nodes.
        router
            .service_manager()
            .set_mesh_services_registry(self.mesh_services_registry.clone());
        router.start();

        if let Err(e) = crate::addon::bundled::install_bundled_addons(db) {
            warn!("Blad instalacji wbudowanych addonow: {}", e);
        }
        if let Err(e) = crate::addon::bundled::install_native_packages(db) {
            warn!("Blad rejestracji natywnych pakietow aplikacji: {}", e);
        }

        let addon_manager = Arc::new(
            AddonManager::new(db.clone(), self.settings_cipher.clone())
                .context("Blad inicjalizacji AddonManager")?,
        );
        addon_manager.set_router(router.clone());
        // Tools and custom flow blocks live only in process memory; without
        // this pass a restart would hide every addon tool from agents until a
        // reinstall or a sync reconcile.
        addon_manager.register_installed_runtimes();
        // Native apps have no WASM runtime to re-register; their boot pass
        // runs `hooks.init`/`on_enable` for every enabled instance, so an engine
        // such as TentaBus comes back after a restart.
        addon_manager.start_installed_native_instances();
        // The boot pass above is the only thing that brings TentaBus engines
        // up, so the running-instance count is complete exactly here.
        crate::bus::native::warn_if_instance_budget_exceeded();
        // A replicated addon instance landing through sync makes the manager
        // load/unload its runtime according to the DB state.
        crate::sync::runtime::set_global_addon_reconciler(addon_manager.clone());
        // Lets a service addon hand slow work to a worker instead of stalling
        // its own tick.
        crate::addon::background::set_manager(&addon_manager);
        router
            .service_manager()
            .set_event_bus(addon_manager.event_bus().clone());

        // Flow operators (Predict, Sink event_publish) must reach the same
        // QUIC routing surface and bus the WASM host functions use.
        {
            let sched = crate::flow_runtime::scheduler::FlowScheduler::global();
            sched.set_service_manager(router.service_manager().clone());
            sched.set_event_bus(addon_manager.event_bus().clone());
            if let Some(executor) = router.executor() {
                sched.set_executor(executor);
            }
        }
        crate::addon::event_publish::init_global(addon_manager.event_bus().clone());

        addon_manager.clone().start_event_dispatcher();

        if let Some(dispatcher) = router.flow_dispatcher() {
            // From here a node_type "addon.{id}.{block}" gets an
            // AddonNodeAdapter instead of "no adapter for node".
            dispatcher.set_addon_resolver(addon_manager.clone());
            info!("FlowDispatcher: addon block resolver wpiety");

            // AddonManager backs the ToolDispatcher and the per-principal tool
            // permission checks of the agent blocks.
            let agent_service = Arc::new(crate::agents::AgentService::new(
                db.clone(),
                addon_manager.clone(),
            ));
            dispatcher.set_agent_service(agent_service);
            info!("FlowDispatcher: AgentService wpiety do slotu");

            // Runs left running/waiting by a previous process are interrupted;
            // a background run is a flow execution on this same dispatcher.
            match crate::agents::AgentRunManager::reap_interrupted_on_startup(db) {
                Ok(n) if n > 0 => {
                    info!("AgentRunManager: {n} orphaned run(s) marked interrupted")
                }
                Ok(_) => {}
                Err(e) => warn!("AgentRunManager: orphan reap failed: {e}"),
            }
            let run_manager = Arc::new(crate::agents::AgentRunManager::from_setting(
                db.clone(),
                Arc::new(crate::agents::FlowDispatcherRunner::new(dispatcher)),
                crate::flow_engine::progress_broker::global_broker(),
            ));
            let run_manager = crate::agents::agent_run_manager_init_global(run_manager);
            dispatcher.set_process_agent_runs(run_manager.clone());

            // Child-completion events become reactive runs of the flows whose
            // entry is `on_subagent_complete`.
            crate::agents::subagent_reactor_init_global(
                db.clone(),
                dispatcher,
                run_manager.child_finished_subscribe(),
            );
            info!("SubagentReactor: zainstalowany (reaktywne flow on_subagent_complete)");

            // Reactive `bus_consume` entry. Safe with no TentaBus instance
            // running yet: each subscription loop tolerates `bus::global()`
            // being `None`.
            crate::bus::reactor::init_global(db.clone(), dispatcher);
            info!("BusReactor: zainstalowany (reaktywne flow bus_consume)");
            // ask_user questions and permission grants raised during a run.
            crate::agents::interaction_registry_init_global(Arc::new(
                crate::agents::InteractionRegistry::new(),
            ));
            info!("AgentRunManager: global registry installed");

            // Redacts expired agent_runs PII and deletes their mailbox entries
            // per the org's retention term; once now, then daily.
            crate::agents::start_agent_runtime_purge_task(db.clone());
        }

        // Spawns only when `curator_interval_hours` is positive; proposals are
        // stored for the dashboard and never auto-applied.
        crate::skills::start_curator_schedule_task(db.clone(), router.clone());

        // Service-mode addons enabled before the restart; otherwise they would
        // run only in the session in which an admin clicked Start.
        addon_manager.auto_start_services();

        Ok(addon_manager)
    }

    /// Starts the mesh pipeline when `[mesh]` enables it and wires the
    /// resulting manager into the router, TentaBus replication, mesh
    /// forwarding and the relays. `startup_timeout` bounds the pipeline start
    /// for hosts that must not wait on the network; a timeout or an error
    /// leaves the node running without mesh.
    pub async fn start_mesh(
        &self,
        addon_manager: &Arc<AddonManager>,
        role: &str,
        startup_timeout: Option<Duration>,
    ) -> Option<MeshPipelineHandles> {
        let Some(mesh_config) = self.config.mesh.as_ref() else {
            info!("Brak konfiguracji mesh");
            return None;
        };
        if !mesh_config.enabled {
            info!("Mesh networking wylaczony w konfiguracji");
            return None;
        }

        let pipeline_config = MeshPipelineConfig {
            node_id: self.local_node_id.clone(),
            role: role.to_string(),
            mesh_config: mesh_config.clone(),
            token_metrics: self.config.token_metrics.clone(),
        };
        let pipeline = start_mesh_pipeline(
            pipeline_config,
            &self.peer_store,
            Some(self.db.clone()),
            self.settings_cipher.clone(),
            self.mesh_security.clone(),
            self.mesh_services_registry.clone(),
        );
        let started = match startup_timeout {
            Some(limit) => match tokio::time::timeout(limit, pipeline).await {
                Ok(started) => started,
                Err(_) => {
                    warn!("Timeout uruchamiania mesh pipeline; dashboard startuje bez mesh handle");
                    return None;
                }
            },
            None => pipeline.await,
        };
        let handles = match started {
            Ok(handles) => handles,
            Err(e) => {
                error!("Blad uruchomienia mesh pipeline: {}", e);
                return None;
            }
        };

        if let Some(ref mesh_mgr) = handles.quic_mesh {
            self.attach_mesh_manager(mesh_mgr, addon_manager).await;
        }
        Some(handles)
    }

    async fn attach_mesh_manager(
        &self,
        mesh_mgr: &Arc<IrohMeshManager>,
        addon_manager: &Arc<AddonManager>,
    ) {
        let router = &self.router;
        // Lets requests be forwarded to remote nodes. The node id is the mesh
        // key from the start, so the peer_store entry needs no swap.
        router.set_mesh_manager(mesh_mgr.clone());

        // Each TentaBus instance starts its own replication from
        // `native_on_enable`, which reaches the mesh through this handle;
        // `replication::router::mesh_manager()` upgrades the `Weak` and stays
        // at RF=1 when the mesh never started or has shut down.
        crate::bus::replication::router::set_mesh_manager(mesh_mgr);

        // `start_installed_native_instances()` already ran `native_on_enable`
        // for every instance enabled before this restart, and each of them saw
        // no mesh manager (it is set for the first time just above), so they
        // came up at RF=1. Re-arm exactly those engines, once — a no-op for
        // instances whose own hook already started replication.
        crate::bus::native::start_replication_for_already_enabled_instances();

        // A remote node uses this router to serve the requests it forwards.
        let router_for_forward = router.clone();
        mesh_mgr
            .set_forward_handler(Arc::new(move |payload: Vec<u8>| {
                let router = router_for_forward.clone();
                Box::pin(async move {
                    use tentaflow_protocol::*;
                    let request: ModelRequest = match tentaflow_protocol::cbor::decode(&payload) {
                        Ok(r) => r,
                        Err(e) => {
                            error!("Forward handler: blad deserializacji ModelRequest: {}", e);
                            let error_response = ModelResponse {
                                request_id: String::new(),
                                result: ModelResult::Error(ErrorInfo {
                                    error_type: ErrorType::InternalError,
                                    message: format!("Forward handler deserialize: {}", e),
                                    details: None,
                                }),
                                metrics: None,
                            };
                            return tentaflow_protocol::cbor::encode(&error_response)
                                .unwrap_or_default();
                        }
                    };

                    let response = crate::mesh::inference_proxy::dispatch_reverse_request(
                        &router, request, None,
                    )
                    .await;

                    tentaflow_protocol::cbor::encode(&response).unwrap_or_default()
                })
            }))
            .await;

        let router_for_stream_forward = router.clone();
        mesh_mgr
            .set_forward_stream_handler(Arc::new(
                move |payload: Vec<u8>, tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>| {
                    let router = router_for_stream_forward.clone();
                    Box::pin(async move {
                        use tentaflow_protocol::*;
                        let request: ModelRequest = match tentaflow_protocol::cbor::decode(&payload)
                        {
                            Ok(r) => r,
                            Err(e) => {
                                error!(
                                    "Forward stream handler: blad deserializacji ModelRequest: {}",
                                    e
                                );
                                let chunk = ModelStreamChunk {
                                    request_id: String::new(),
                                    chunk: StreamChunkType::Error(ErrorInfo {
                                        error_type: ErrorType::InternalError,
                                        message: format!("Forward stream deserialize: {}", e),
                                        details: None,
                                    }),
                                };
                                if let Ok(bytes) = tentaflow_protocol::cbor::encode(&chunk) {
                                    let _ = tx.send(bytes);
                                }
                                return;
                            }
                        };
                        crate::mesh::inference_proxy::dispatch_reverse_stream_request(
                            &router, request, tx, None,
                        )
                        .await;
                    })
                },
            ))
            .await;

        // Owner side of the live camera relay: a trusted observer opens a
        // bi-stream for `camera:<id>` and gets fMP4 frames from the local
        // StreamHub. The captured mesh id feeds the owner-side org gate.
        #[cfg(feature = "camera")]
        {
            let camera_relay_node_id = mesh_mgr.node_id();
            mesh_mgr
                .set_camera_stream_handler(Arc::new(
                    move |payload: Vec<u8>, tx: tokio::sync::mpsc::Sender<Vec<u8>>| {
                        let local_node_id = camera_relay_node_id.clone();
                        Box::pin(async move {
                            crate::services::camera_relay::server::handle(
                                payload,
                                tx,
                                local_node_id,
                            )
                            .await;
                        })
                    },
                ))
                .await;
        }

        // Owner side of the live LiDAR relay (`lidar:<robot_id>`). Not behind
        // the `camera` feature: a robot may carry LiDAR without a camera.
        let lidar_relay_node_id = mesh_mgr.node_id();
        mesh_mgr
            .set_lidar_stream_handler(Arc::new(
                move |payload: Vec<u8>, tx: tokio::sync::mpsc::Sender<Vec<u8>>| {
                    let local_node_id = lidar_relay_node_id.clone();
                    Box::pin(async move {
                        crate::services::lidar_relay::server::handle(payload, tx, local_node_id)
                            .await;
                    })
                },
            ))
            .await;

        if let Some(port_allocator) = self.port_allocator.clone() {
            if let Some(executor) = mesh_mgr.command_executor().await {
                executor
                    .set_service_action_context(
                        crate::mesh::command_executor::ServiceActionContext {
                            db: self.db.clone(),
                            port_allocator,
                            iroh: mesh_mgr.clone(),
                            router: router.clone(),
                            addon_manager: addon_manager.clone(),
                        },
                    )
                    .await;
            }
        }

        // Sender side of `robot_dispatch_v1`: routes a controller action to
        // the node that physically owns the robot.
        crate::mesh::robot_dispatch::set_dispatch_context(
            crate::mesh::robot_dispatch::RobotDispatchContext {
                iroh: mesh_mgr.clone(),
                addon_manager: addon_manager.clone(),
                local_node_id: mesh_mgr.node_id(),
            },
        );

        info!("Mesh routing podlaczony do routera");
    }

    /// Starts the metrics collector, the SLA evaluator and the local metrics
    /// refresher, and marks deployments interrupted by an unclean shutdown. A
    /// host that can resume deployments does so AFTER this call.
    pub async fn start_background_services(
        &mut self,
        mesh: Option<&MeshPipelineHandles>,
    ) -> Arc<RouterMetrics> {
        let metrics = RouterMetrics::new();
        let collector = MetricsCollector::new(metrics.clone(), Some(self.db.clone()));
        collector
            .start(self.router.service_manager().shutdown_rx.clone())
            .await;

        // TB-03: judges every governed flow against its SLA policy every 30 s and
        // keeps the breaches and minute samples. Stops with the router's shutdown
        // signal, like the collector above; the handle is awaited before the WAL
        // checkpoint so its final audit flush is not lost.
        self.sla_evaluator = Some(crate::sla::evaluator::spawn(
            self.db.clone(),
            self.local_node_id.clone(),
            self.router.service_manager().shutdown_rx.clone(),
        ));

        // Without a running mesh nothing refreshes the local node's live
        // metrics: the heartbeat sender does it only while broadcasting.
        if mesh.and_then(|m| m.quic_mesh.as_ref()).is_none() {
            crate::mesh::pipeline::spawn_local_metrics_refresher(
                self.peer_store.clone(),
                self.local_node_id.clone(),
            );
        }

        // The task that produced a deploying row died with the previous
        // process, so the row is marked interrupted.
        match db::repository::deployments::reset_stale(&self.db) {
            Ok(n) if n > 0 => info!(
                "Deployments cleanup: {} stale rows marked as interrupted",
                n
            ),
            Ok(_) => {}
            Err(e) => warn!("Deployments cleanup: {}", e),
        }
        metrics
    }

    /// Starts the BPMN process runtime and the unified HTTPS server (OpenAI
    /// API + dashboard on one port).
    pub fn start_server(
        &self,
        metrics: &Arc<RouterMetrics>,
        addon_manager: &Arc<AddonManager>,
        mesh: Option<&MeshPipelineHandles>,
    ) -> Result<()> {
        if let Some(dispatcher) = self.router.flow_dispatcher() {
            crate::processes::runtime::start(&self.db, dispatcher)?;
        }

        crate::api::unified_server::start_unified_server(
            &self.config,
            &self.db,
            metrics,
            &self.router,
            &self.peer_store,
            mesh.and_then(|m| m.quic_mesh.clone()),
            Arc::from(self.local_node_id.as_str()),
            mesh.and_then(|m| m.security.clone()),
            Some(addon_manager.clone()),
            mesh.map(|m| m.relay_health.clone()),
            self.port_allocator.clone(),
            self.mesh_services_registry.clone(),
        )
    }

    /// First step of a graceful shutdown: the BPMN process runtime stops
    /// taking work. A host stops its own workers between this and
    /// [`CoreNode::shutdown`].
    pub async fn stop_process_runtime(&self) -> Result<()> {
        if let Some(dispatcher) = self.router.flow_dispatcher() {
            crate::processes::runtime::stop(dispatcher).await?;
        }
        Ok(())
    }

    /// The rest of a graceful shutdown, in dependency order: addons and their
    /// pending durable state, supervised engines, agent bridges, the router,
    /// TentaBus, cameras, the mesh, the SLA evaluator's last audit flush, and
    /// finally the WAL checkpoint of every database. `mesh_timeout` bounds the mesh shutdown for hosts whose
    /// process is killed after a fixed grace period.
    pub async fn shutdown(
        &mut self,
        addon_manager: &AddonManager,
        mesh: Option<MeshPipelineHandles>,
        mesh_timeout: Option<Duration>,
    ) {
        // Cancels the service tick loops and breaks the Arc<AddonManager>
        // cycle held by the event dispatcher; without it the process does not
        // exit.
        addon_manager.shutdown();
        // Bounded, so a stuck DB never hangs shutdown.
        if let Err(e) = addon_manager
            .await_state_flusher_drain(Duration::from_secs(10))
            .await
        {
            warn!("addon state flusher drain on shutdown: {}", e);
        }
        // Supervised engines (native python-bundle / binary / docker) stop
        // before the router releases its locks, or vLLM/SGLang children stay
        // behind holding VRAM. The health loop stops FIRST so it cannot
        // respawn the engines being killed.
        if let Some(flag) = &self.supervisor_shutdown {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(ports) = self.port_allocator.clone() {
            let errors = crate::services::deploy::stop_all_supervised(&self.db, ports).await;
            for (id, msg) in &errors {
                warn!("shutdown stop service id={}: {}", id, msg);
            }
        }
        // Agent bridges are not `services` rows. One left running holds its
        // account's `account.lock` and credential directory, so the next Core
        // could never start that account.
        let stopped = crate::services::agent_runtime::stop_all().await;
        if stopped > 0 {
            info!("stopped {} agent account bridge(s) on shutdown", stopped);
        }
        self.router.shutdown();

        // Replication stops before the retention sweepers so no follower
        // observes a half-torn-down leader; every running instance has its
        // own manager. Stopping an instance flushes its pending windowed audit
        // occurrences now, not at the sweeper's next tick.
        for manager in crate::bus::replication::router::running_managers() {
            crate::bus::replication::init::stop(&manager);
        }
        for bus_service in crate::bus::running_instances() {
            bus_service.shutdown();
        }

        // GStreamer pipelines must end before the runtime does, or their EOS
        // messages race the tokio worker teardown.
        #[cfg(feature = "camera")]
        crate::addon::host_functions::camera::shutdown_camera_supervisor_global().await;

        // Closes the QUIC endpoint (frees the UDP port) and unregisters mDNS.
        if let Some(mesh) = mesh {
            match mesh_timeout {
                Some(limit) => {
                    if tokio::time::timeout(limit, mesh.shutdown()).await.is_err() {
                        warn!("Timeout mesh shutdown ({:?})", limit);
                    }
                }
                None => mesh.shutdown().await,
            }
        }

        // The evaluator stopped with the router's shutdown signal; let it write its
        // pending audit rows before the checkpoint (bounded, a stuck round must not
        // hang the shutdown).
        if let Some(sla_evaluator) = self.sla_evaluator.take() {
            if tokio::time::timeout(crate::sla::evaluator::SHUTDOWN_GRACE, sla_evaluator)
                .await
                .is_err()
            {
                warn!("SLA evaluator did not stop in time; its last audit rows may be lost");
            }
        }

        if let Err(e) = db::checkpoint_wal(&self.db) {
            warn!("Checkpoint WAL nieudany: {}", e);
        }
        if let Err(e) = crate::ml_studio::db::checkpoint_wal() {
            warn!("Checkpoint WAL ML Studio nieudany: {}", e);
        }
        if let Err(e) = crate::services::ingest_jobs::checkpoint_wal() {
            error!("Ingest queue WAL checkpoint failed: {}", e);
        }
        if let Err(e) = crate::project_studio::db::checkpoint_wal() {
            warn!("Checkpoint WAL Project Studio nieudany: {}", e);
        }
        crate::project_studio::project_db::checkpoint_all();
        crate::addon::app_db::checkpoint_all();
        // A scope task still draining the progress broadcast would append
        // behind the truncation.
        crate::events::progress_log::stop();
        if let Err(e) = crate::events::db::checkpoint_wal() {
            warn!("Event log WAL checkpoint failed: {}", e);
        }
    }
}
