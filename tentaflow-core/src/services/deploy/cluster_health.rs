// =============================================================================
// Plik: services/deploy/cluster_health.rs
// Opis: Zycie deploymentu klastra POZA procesem, ktory go uruchomil. Fazy P0-P6
//       pilnuje zadanie tokio zabijane razem z procesem, wiec bez tego modulu
//       restart TentaFlow zostawial rekord w stanie, ktorego nikt juz nie
//       weryfikowal: `deploying` w nieskonczonosc albo `running` mimo martwego
//       kontenera. Supervisor serwisow celowo pomija czlonkow distributed
//       (headless worker zawsze wypadlby jako awaria przy lokalnej sondzie),
//       wiec zdrowie klastra nie mialo ZADNEGO wlasciciela.
// Przyklad: reconcile_on_startup(&db, &local_node_id); spawn_health_loop(...)
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use crate::db::DbPool;

/// Ile kolejnych nieudanych sond zanim `running` stanie sie `failed`. Sonda leci
/// co `HEALTH_INTERVAL`, wiec to jest realny czas tolerancji na chwilowa
/// niedostepnosc (restart kontenera, przeciazenie).
const FAILURES_BEFORE_FAILED: u32 = 3;

/// Odstep miedzy sondami zdrowia.
const HEALTH_INTERVAL: Duration = Duration::from_secs(30);

/// Domyslny port Ray GCS — ten sam, ktorego uzywa deploy.
const RAY_PORT: u16 = 6379;

/// Rekoncyliacja przy starcie procesu.
///
/// `deploying` — fazy prowadzilo zadanie, ktore zginelo razem z poprzednim
/// procesem. Nikt go nie dokonczy, a rekord blokuje kolejny deploy na tym
/// klastrze (`active_cluster_deployment` liczy `deploying` jako aktywny), wiec
/// oznaczamy `failed`: deploy faktycznie sie nie udal.
///
/// `running` — TentaFlow celowo ubija zarzadzane silniki przy zamykaniu, wiec
/// po restarcie serwer NIE dziala, mimo ze kontener (trzymany przez
/// `sleep infinity`) zyje dalej. To jest `stopped`, nie `failed`: nic nie
/// padlo, deployment zostal wylaczony razem z programem. Rozroznienie jest dla
/// czlowieka patrzacego na dashboard — `failed` kazalby szukac awarii, ktorej
/// nie bylo. Status `failed` zostaje dla modelu, ktory padl SAM, przy dzialajacym
/// TentaFlow; to wykrywa petla zdrowia.
pub fn reconcile_on_startup(db: &DbPool) {
    for (status, next, note) in [
        ("deploying", "failed", "deploy przerwany restartem procesu"),
        ("running", "stopped", "silnik wylaczony razem z TentaFlow"),
    ] {
        let rows = match crate::db::repository::cluster_deployments_by_status(db, &[status]) {
            Ok(v) => v,
            Err(e) => {
                warn!("cluster health: nie moge odczytac deploymentow: {}", e);
                return;
            }
        };
        for d in rows {
            info!(
                deployment_cluster_id = %d.deployment_cluster_id,
                model = %d.model,
                "cluster health: {} — {} -> {}", note, status, next
            );
            if let Err(e) = crate::db::repository::set_cluster_deployment_status(
                db,
                &d.deployment_cluster_id,
                next,
            ) {
                warn!("cluster health: zapis statusu nieudany: {}", e);
            }
        }
    }
}

/// Petla zdrowia deploymentow klastra. Sonduje TYLKO te, dla ktorych ten node
/// jest headem — to on trzyma endpoint OpenAI, a worker jest headless i lokalna
/// sonda zawsze pokazalaby go jako martwego.
pub fn spawn_health_loop(db: DbPool, local_node_id: Arc<str>) {
    tokio::spawn(async move {
        // Licznik kolejnych porazek per deployment. Trzymany w pamieci: po
        // restarcie zaczynamy od zera, co jest wlasciwe — swiezy proces nie
        // powinien dziedziczyc podejrzen poprzedniego.
        let mut failures: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        loop {
            tokio::time::sleep(HEALTH_INTERVAL).await;
            let deployments = match crate::db::repository::cluster_deployments_by_status(
                &db,
                &["running", "degraded"],
            ) {
                Ok(v) => v,
                Err(e) => {
                    warn!("cluster health: odczyt deploymentow: {}", e);
                    continue;
                }
            };
            let live: std::collections::HashSet<String> = deployments
                .iter()
                .map(|d| d.deployment_cluster_id.clone())
                .collect();
            failures.retain(|k, _| live.contains(k));

            for d in deployments {
                if d.head_node_id.as_str() != &*local_node_id {
                    continue;
                }
                let s = crate::services::deploy::distributed::probe_readiness(
                    &d.deployment_cluster_id,
                    RAY_PORT,
                    d.port as u16,
                )
                .await;
                let entry = failures.entry(d.deployment_cluster_id.clone()).or_insert(0);
                if s.serve_ready {
                    if *entry > 0 || d.status != "running" {
                        info!(
                            deployment_cluster_id = %d.deployment_cluster_id,
                            "cluster health: klaster znowu serwuje"
                        );
                        let _ = crate::db::repository::set_cluster_deployment_status(
                            &db,
                            &d.deployment_cluster_id,
                            "running",
                        );
                    }
                    *entry = 0;
                    continue;
                }
                *entry += 1;
                warn!(
                    deployment_cluster_id = %d.deployment_cluster_id,
                    container_running = s.container_running,
                    ray_gcs_up = s.ray_gcs_up,
                    strike = *entry,
                    "cluster health: /v1/models nie odpowiada"
                );
                let next = if *entry >= FAILURES_BEFORE_FAILED {
                    "failed"
                } else {
                    "degraded"
                };
                if d.status != next {
                    let _ = crate::db::repository::set_cluster_deployment_status(
                        &db,
                        &d.deployment_cluster_id,
                        next,
                    );
                }
            }
        }
    });
}
