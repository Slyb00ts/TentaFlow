// =============================================================================
// Plik: services/deploy/cluster_health.rs
// Opis: Zycie deploymentu klastra POZA procesem, ktory go uruchomil.
//       Odpowiada za rekoncyliacje po restarcie procesu, nadzor zdrowia klastra
//       oraz automatyczne, zsynchronizowane wznawianie modelu w oparciu o stan
//       polaczen partnerow w iroh mesh.
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use crate::db::DbPool;

/// Domyslny port Ray GCS — ten sam, ktorego uzywa deploy.
const RAY_PORT: u16 = 6379;

/// Pomocnik: aktualizuje status serwisu powiazanego z danym deploymentem klastra.
fn update_service_status_for_deployment(
    db: &DbPool,
    deployment_cluster_id: &str,
    status: crate::services_repo::services::ServiceStatus,
) {
    let service_id = {
        let conn = match db.read() {
            Ok(c) => c,
            Err(_) => return,
        };
        match crate::services_repo::services::find_id_by_active_deploy_id(&conn, deployment_cluster_id) {
            Ok(Some(id)) => id,
            _ => {
                let mut stmt = match conn.prepare(
                    "SELECT id FROM services WHERE config_json LIKE '%' || ?1 || '%' LIMIT 1"
                ) {
                    Ok(s) => s,
                    Err(_) => return,
                };
                match stmt.query_row(rusqlite::params![deployment_cluster_id], |row| row.get::<_, i64>(0)) {
                    Ok(id) => id,
                    Err(_) => return,
                }
            }
        }
    };
    if let Ok(conn) = db.write() {
        let _ = crate::services_repo::services::update_status(&conn, service_id, status);
    }
}

/// Rekoncyliacja przy starcie procesu.
///
/// `deploying` — deploy byl w trakcie wykonywania w zadanym tokio tasku, ktory
/// zginal. Oznaczamy jako `failed`.
///
/// `running`, `starting`, `stopped` — po restarcie serwera silnik nie serwuje od razu,
/// ale zamiast porzucac deployment jako trwale `stopped`, oznaczamy go jako `degraded`
/// i pozwalamy petli zdrowia na weryfikacje obecnosci peerow w mesh i automatyczny start.
pub fn reconcile_on_startup(db: &DbPool) {
    // 1. Deploy przerwany restartem procesu -> failed
    if let Ok(deploying) = crate::db::repository::cluster_deployments_by_status(db, &["deploying"]) {
        for d in deploying {
            info!(
                deployment_cluster_id = %d.deployment_cluster_id,
                "cluster health: deploy przerwany restartem procesu — deploying -> failed"
            );
            let _ = crate::db::repository::set_cluster_deployment_status(
                db,
                &d.deployment_cluster_id,
                "failed",
            );
            update_service_status_for_deployment(
                db,
                &d.deployment_cluster_id,
                crate::services_repo::services::ServiceStatus::Failed,
            );
        }
    }

    // 2. Aktywne deploymenty -> degraded, zeby petla zdrowia podjela probe wznowienia
    if let Ok(active) = crate::db::repository::cluster_deployments_by_status(
        db,
        &["running", "starting", "stopped"],
    ) {
        for d in active {
            info!(
                deployment_cluster_id = %d.deployment_cluster_id,
                old_status = %d.status,
                "cluster health: rekoncyliacja po starcie — oczekiwanie na gotowosc mesh -> degraded"
            );
            let _ = crate::db::repository::set_cluster_deployment_status(
                db,
                &d.deployment_cluster_id,
                "degraded",
            );
            update_service_status_for_deployment(
                db,
                &d.deployment_cluster_id,
                crate::services_repo::services::ServiceStatus::Degraded,
            );
        }
    }
}

/// Petla zdrowia i rekoncyliacji deploymentow klastra.
///
/// Koordynator (head node) monitoruje polaczenia w iroh mesh ze wszystkimi nodami
/// czlonkowskimi. Gdy nody sa online, koordynuje zsynchronizowany start modelu
/// (restart kontenera workera przez mesh + exec rank 0 na headzie).
pub fn spawn_health_loop(
    db: DbPool,
    local_node_id: Arc<str>,
    router: Arc<crate::routing::Router>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        let mut last_start_attempt: std::collections::HashMap<String, std::time::Instant> =
            std::collections::HashMap::new();

        loop {
            interval.tick().await;

            let deployments = match crate::db::repository::cluster_deployments_by_status(
                &db,
                &["running", "degraded", "starting", "stopped"],
            ) {
                Ok(v) => v,
                Err(e) => {
                    warn!("cluster health: odczyt deploymentow: {}", e);
                    continue;
                }
            };

            for d in deployments {
                if d.head_node_id.as_str() != &*local_node_id {
                    continue;
                }

                // 1. Sprawdz czy model juz serwuje (/v1/models zwraca 200 OK)
                let s = crate::services::deploy::distributed::probe_readiness(
                    &d.deployment_cluster_id,
                    RAY_PORT,
                    d.port as u16,
                )
                .await;

                if s.serve_ready {
                    if d.status != "running" {
                        info!(
                            deployment_cluster_id = %d.deployment_cluster_id,
                            "cluster health: model serwuje poprawnie na headzie -> status running"
                        );
                        let _ = crate::db::repository::set_cluster_deployment_status(
                            &db,
                            &d.deployment_cluster_id,
                            "running",
                        );
                        update_service_status_for_deployment(
                            &db,
                            &d.deployment_cluster_id,
                            crate::services_repo::services::ServiceStatus::Running,
                        );
                    }
                    last_start_attempt.remove(&d.deployment_cluster_id);
                    continue;
                }

                // 2. Model nie serwuje. Sprawdz czy mamy dostep do mesh managera.
                let qm = match router.mesh_manager() {
                    Some(qm) => qm,
                    None => continue,
                };

                // 3. Sprawdz czlonkow klastra
                let members = match crate::db::repository::list_cluster_deployment_members(
                    &db,
                    &d.deployment_cluster_id,
                ) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!("cluster health: nie moge pobrac czlonkow deploymentu: {}", e);
                        continue;
                    }
                };

                // Sprawdz czy WSZYSCY partnerzy (workery) sa online w iroh mesh
                let mut all_peers_online = true;
                let mut offline_peer = String::new();
                for m in &members {
                    if m.node_id == *local_node_id {
                        continue;
                    }
                    if !qm.is_connected(&m.node_id).await {
                        all_peers_online = false;
                        offline_peer = m.node_id.clone();
                        break;
                    }
                }

                if !all_peers_online {
                    if d.status != "degraded" {
                        info!(
                            deployment_cluster_id = %d.deployment_cluster_id,
                            offline_peer = %offline_peer,
                            "cluster health: peer offline w mesh — oznaczam degraded i czekam na polaczenie noda"
                        );
                        let _ = crate::db::repository::set_cluster_deployment_status(
                            &db,
                            &d.deployment_cluster_id,
                            "degraded",
                        );
                        update_service_status_for_deployment(
                            &db,
                            &d.deployment_cluster_id,
                            crate::services_repo::services::ServiceStatus::Degraded,
                        );
                    }
                    continue;
                }

                // 4. Wszyscy partnerzy sa ONLINE w mesh!
                // Sprawdz czy proces serwowania na headzie juz zyje (np. trwa ladowanie wag / rozgrzewka)
                let serve_alive = crate::services::deploy::distributed::is_serve_process_alive(
                    &d.deployment_cluster_id,
                )
                .await;

                if serve_alive {
                    // Proces zyje i sie laduje.
                    let starting_too_long = last_start_attempt
                        .get(&d.deployment_cluster_id)
                        .map(|t| t.elapsed() > Duration::from_secs(900))
                        .unwrap_or(false);

                    if !starting_too_long {
                        if d.status != "starting" {
                            let _ = crate::db::repository::set_cluster_deployment_status(
                                &db,
                                &d.deployment_cluster_id,
                                "starting",
                            );
                            update_service_status_for_deployment(
                                &db,
                                &d.deployment_cluster_id,
                                crate::services_repo::services::ServiceStatus::Starting,
                            );
                        }
                        // Czekamy na zakonczenie ladowania modelu
                        continue;
                    }

                    warn!(
                        deployment_cluster_id = %d.deployment_cluster_id,
                        "cluster health: proces serwowania dziala ponad 5 minut bez wystawienia portu — resetuje"
                    );
                }

                // 5. Proces nie zyje (albo powiesil sie na dluzej niz 5 min) i wszyscy partnerzy sa online.
                // Cooldown: min. 15 sekund miedzy probami startu
                if let Some(t) = last_start_attempt.get(&d.deployment_cluster_id) {
                    if t.elapsed() < Duration::from_secs(15) {
                        continue;
                    }
                }

                info!(
                    deployment_cluster_id = %d.deployment_cluster_id,
                    "cluster health: wszyscy partnerzy online w mesh — uruchamiam zsynchronizowany restart klastra"
                );
                last_start_attempt.insert(d.deployment_cluster_id.clone(), std::time::Instant::now());

                let _ = crate::db::repository::set_cluster_deployment_status(
                    &db,
                    &d.deployment_cluster_id,
                    "starting",
                );
                update_service_status_for_deployment(
                    &db,
                    &d.deployment_cluster_id,
                    crate::services_repo::services::ServiceStatus::Starting,
                );

                // Krok A: Zrestartuj kontenery workerow przez komende mesh
                for m in &members {
                    if m.node_id == *local_node_id {
                        continue;
                    }
                    let container = if !m.container_name.is_empty() {
                        m.container_name.clone()
                    } else {
                        crate::services::deploy::distributed::container_name(&d.engine_id, d.port as u16)
                    };

                    info!(
                        deployment_cluster_id = %d.deployment_cluster_id,
                        worker_node = %m.node_id,
                        container = %container,
                        "cluster health: restartuje kontener workera przez mesh"
                    );
                    let restart_cmd = tentaflow_protocol::mesh::MeshCommandType::ContainerRestart {
                        container_id: container.clone(),
                    };
                    let res = qm.send_command(&m.node_id, restart_cmd).await;
                    if res.as_ref().map_or(false, |r| !r.ok) {
                        let start_cmd = tentaflow_protocol::mesh::MeshCommandType::ContainerStart {
                            container_id: container.clone(),
                        };
                        let _ = qm.send_command(&m.node_id, start_cmd).await;
                    }
                }

                // Krok B: Upewnij sie ze kontener head-a dziala i wyczysc stary proces serve
                if let Err(e) = crate::services::deploy::distributed::ensure_head_container_running(
                    &d.deployment_cluster_id,
                )
                .await
                {
                    warn!("cluster health: ensure_head_container_running: {}", e);
                    continue;
                }

                let _ = crate::services::deploy::distributed::reset_head_serve_process(
                    &d.deployment_cluster_id,
                )
                .await;

                // Krok C: Daj workerom 3 sekundy na start kontenera i wejscie w petle polaczenia TCPStore
                tokio::time::sleep(Duration::from_secs(8)).await;

                // Krok D: Zbuduj komende serve dla heada i odpal przez exec_serve_on_head
                let serve_cmd = match crate::services::deploy::distributed::build_head_serve_cmd(
                    &db,
                    &d,
                ) {
                    Ok(cmd) => cmd,
                    Err(e) => {
                        warn!("cluster health: build_head_serve_cmd: {}", e);
                        continue;
                    }
                };

                info!(
                    deployment_cluster_id = %d.deployment_cluster_id,
                    "cluster health: odpalam rank 0 na headzie przez exec_serve_on_head"
                );
                if let Err(e) = crate::services::deploy::distributed::exec_serve_on_head(
                    &d.deployment_cluster_id,
                    &serve_cmd,
                )
                .await
                {
                    warn!("cluster health: exec_serve_on_head nie powiodl sie: {}", e);
                }
            }
        }
    });
}
