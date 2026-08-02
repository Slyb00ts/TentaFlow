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

/// Rekoncyliacja przy starcie procesu. Deployment w stanie `deploying` byl
/// prowadzony przez zadanie, ktore zginelo razem z poprzednim procesem — nikt
/// go nie dokonczy, a rekord blokuje kolejny deploy na tym klastrze
/// (`active_cluster_deployment` liczy `deploying` jako aktywny). Oznaczamy go
/// `failed`, zeby admin zobaczyl prawde i mogl ponowic.
///
/// Rekordow `running` NIE ruszamy tutaj: kontener przezywa restart TentaFlow,
/// wiec deployment moze byc w pelni sprawny. Weryfikuje je petla zdrowia, ktora
/// daje im czas na odpowiedz zamiast skazywac je na podstawie jednej sondy w
/// momencie bootu.
pub fn reconcile_on_startup(db: &DbPool) {
    let interrupted = match crate::db::repository::cluster_deployments_by_status(db, &["deploying"])
    {
        Ok(v) => v,
        Err(e) => {
            warn!("cluster health: nie moge odczytac deploymentow: {}", e);
            return;
        }
    };
    for d in interrupted {
        warn!(
            deployment_cluster_id = %d.deployment_cluster_id,
            model = %d.model,
            "cluster health: deploy przerwany restartem procesu — oznaczam jako failed"
        );
        if let Err(e) = crate::db::repository::set_cluster_deployment_status(
            db,
            &d.deployment_cluster_id,
            "failed",
        ) {
            warn!("cluster health: zapis statusu nieudany: {}", e);
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
