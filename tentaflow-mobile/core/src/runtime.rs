// =============================================================================
// Plik: runtime.rs
// Opis: Runtime mobilny wzorowany na Desktop — uruchamia Router, unified HTTPS
//       server, mesh pipeline (mDNS + QUIC), metryki, agenty. Bez egui UI —
//       dashboard serwowany przez unified server w przegladarce.
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tentaflow_core::config::NodeConfig;
use tentaflow_core::db;
use tentaflow_core::mesh::peer_store::MeshPeerStore;
use tentaflow_core::mesh::pipeline::{
    start_mesh_pipeline, MeshPipelineConfig, MeshPipelineHandles,
};
use tentaflow_core::mesh::security::MeshSecurity;
use tentaflow_core::metrics::{collector::MetricsCollector, RouterMetrics};
use tentaflow_core::routing::Router;
use tentaflow_ui::state::SharedAppState;
use tokio::sync::watch;
use tracing::{error, info, warn};

/// Uchwyty do uruchomionych serwisow — potrzebne do graceful shutdown
pub struct ServiceHandles {
    /// Kanal sygnalizujacy zamkniecie
    shutdown_tx: watch::Sender<bool>,
    /// Router — do wywolania shutdown()
    router: Option<Arc<Router>>,
    /// Uchwyty mesh pipeline — MUSZA zyc, bo Drop wyrejestruje mDNS
    mesh_handles: Option<MeshPipelineHandles>,
}

/// Uruchamia wszystkie serwisy Core w tle (wzor Desktop)
pub async fn start_services(config: NodeConfig, _state: SharedAppState) -> Result<ServiceHandles> {
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);

    info!("Uruchamianie serwisow Core (tryb mobilny)...");

    // Inicjalizacja bazy danych SQLite
    let data_dir = crate::platform::data_dir();
    std::fs::create_dir_all(&data_dir)?;

    // Diagnostyka: czy to wake po suspend, czy cold start po terminate?
    // Czytamy last_alive.txt pisane przez heartbeat_task w poprzedniej instancji.
    crate::diagnostics::diagnose_startup(&data_dir);

    let db_path = data_dir.join("mobile.db");
    info!(path = %db_path.display(), "Baza danych SQLite");

    let db = db::init(&db_path).map_err(|e| {
        error!("Blad inicjalizacji bazy danych: {}", e);
        e
    })?;
    info!("Baza danych zainicjalizowana");

    // Czyszczenie osieroconego settings.node_id (legacy UUID) — zastapiony
    // iroh EndpointId z MeshSecurity.public_key_hex().
    let _ = db::repository::delete_setting(&db, "node_id");

    // Inicjalizacja routera
    info!("Inicjalizacja routera...");
    let router = Arc::new(Router::new(config.clone(), Some(db.clone()))?);
    router.start();
    info!("Router uruchomiony");

    // Zaladuj serwisy z bazy danych (wspolna metoda Core)
    router.load_db_services();

    // Przywroc natywne serwisy (in-process MLX/llama.cpp) z bazy
    router.restore_native_services().await;

    // Zainstaluj wbudowane addony (WASM — wasmi interpreter na mobile)
    if let Err(e) = tentaflow_core::addon::bundled::install_bundled_addons(&db) {
        tracing::warn!("Blad instalacji wbudowanych addonow: {}", e);
    }

    // Inicjalizacja metryk
    let metrics = RouterMetrics::new();
    let collector = MetricsCollector::new(metrics.clone(), Some(db.clone()));
    collector.start(shutdown_tx.subscribe()).await;

    // Heartbeat diagnostyki — co 5s zapisuje timestamp do last_alive.txt,
    // zeby nastepny startup mogl rozroznic suspend od terminate.
    crate::diagnostics::spawn_heartbeat_task(data_dir.clone(), shutdown_tx.subscribe());

    // Ladowanie master key z pliku i inicjalizacja SettingsCipher
    let file_master_key = tentaflow_core::crypto::load_or_create_master_key_in(Some(&data_dir))
        .expect("Nie udalo sie zaladowac master key z pliku");
    let settings_cipher = Arc::new(tentaflow_core::crypto::SettingsCipher::new(
        &file_master_key,
    ));

    // Migracja istniejacych plaintextowych sekretow
    match tentaflow_core::crypto::migrate_plaintext_secrets(&db, &settings_cipher) {
        Ok(n) if n > 0 => info!("Zaszyfrowano {} plaintextowych sekretow w bazie", n),
        Err(e) => error!("Blad migracji sekretow: {}", e),
        _ => {}
    }

    // MeshSecurity — single source of truth dla tozsamosci. Ed25519 keypair
    // jest zapisany zaszyfrowany w settings; iroh uzywa tego klucza jako
    // EndpointId. Dashboard mesh porownuje node_id po Ed25519 hex.
    let mesh_security = Arc::new(
        MeshSecurity::new(db.clone(), settings_cipher.clone())
            .map_err(|e| {
                error!("MeshSecurity init: {}", e);
                e
            })?,
    );
    let node_id = mesh_security.ed25519_public_key_hex();
    info!("Mesh identity: {}", &node_id[..16.min(node_id.len())]);

    // Store peerow mesh — wspoldzielony miedzy mDNS, QUIC, dashboard
    let mesh_peer_store = MeshPeerStore::new();

    // Mesh networking — mDNS discovery + QUIC mesh (wspoldzielony pipeline z Core)
    let mesh_handles: Option<MeshPipelineHandles>;
    let mesh_enabled = config.mesh.as_ref().map_or(false, |m| m.enabled);

    if mesh_enabled {
        let pipeline_config = MeshPipelineConfig {
            node_id: node_id.clone(),
            role: "mobile".to_string(),
            mesh_config: config.mesh.as_ref().unwrap().clone(),
        };

        match start_mesh_pipeline(
            pipeline_config,
            &mesh_peer_store,
            Some(db.clone()),
            settings_cipher.clone(),
            mesh_security.clone(),
        )
        .await
        {
            Ok(handles) => {
                info!("Mesh pipeline uruchomiony");

                // Eksponuj IrohMeshManager + tokio runtime handle do FFI — Swift
                // NWBrowser bedzie karmic iroh peerami znalezionymi przez Bonjour.
                if let Some(ref quic_mesh) = handles.quic_mesh {
                    crate::ffi_discovery::set_mesh_handle(quic_mesh.clone());
                    crate::ffi_discovery::set_mesh_runtime(tokio::runtime::Handle::current());
                }

                mesh_handles = Some(handles);
            }
            Err(e) => {
                error!("Blad uruchomienia mesh pipeline: {}", e);
                mesh_handles = None;
            }
        }
    } else {
        info!("Mesh networking wylaczony w konfiguracji");
        mesh_handles = None;
    };

    // Unified HTTPS server (OpenAI API + Dashboard na jednym porcie) — z Core
    let quic_mesh_for_server = mesh_handles.as_ref().and_then(|h| h.quic_mesh.clone());
    let mesh_security_for_server = mesh_handles.as_ref().and_then(|h| h.security.clone());
    let local_node_id: Arc<str> = Arc::from(node_id.as_str());

    tentaflow_core::api::unified_server::start_unified_server(
        &config,
        &db,
        &metrics,
        &router,
        &mesh_peer_store,
        quic_mesh_for_server,
        local_node_id,
        mesh_security_for_server,
    )?;

    info!("Wszystkie serwisy uruchomione (tryb mobilny)");

    Ok(ServiceHandles {
        shutdown_tx,
        router: Some(router),
        mesh_handles,
    })
}

/// Zatrzymuje wszystkie serwisy (graceful shutdown z timeoutem)
pub async fn shutdown(handles: ServiceHandles) {
    info!("Rozpoczynanie graceful shutdown...");

    // Zatrzymaj router
    if let Some(ref router) = handles.router {
        router.shutdown();
    }

    // Wyslij sygnal zamkniecia do wszystkich taskow
    let _ = handles.shutdown_tx.send(true);

    // Graceful shutdown mesh — zamyka QUIC endpoint i wyrejestruje mDNS
    if let Some(mesh) = handles.mesh_handles {
        match tokio::time::timeout(Duration::from_secs(5), mesh.shutdown()).await {
            Ok(_) => info!("Mesh shutdown zakonczony"),
            Err(_) => warn!("Timeout mesh shutdown (5s)"),
        }
    }

    info!("Graceful shutdown zakonczony");
}
