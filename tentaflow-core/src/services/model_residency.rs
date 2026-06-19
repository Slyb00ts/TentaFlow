// =============================================================================
// File: services/model_residency.rs — lazy-load + memory guard dla embedded
// =============================================================================
// Zarzadza rezydencja UNPINNED modeli embedded (LLM/STT/TTS) w pamieci:
//   * lazy-load na pierwsze zadanie (`ensure_loaded`) — reuse `deploy::respawn`
//     (cala logika download/load/register z configu serwisu, bez forka),
//   * single-resident eviction — przed zaladowaniem nowego modelu wyladowuje
//     pozostale rezydentne (telefon trzyma max jeden duzy model naraz),
//   * idle-unload — watek w tle zwalnia modele bezczynne > `idle_timeout`,
//   * `unload_all` dla memory guard (ostrzezenie o niskiej pamieci iOS/Android).
//
// PINNED serwisy NIE sa zarzadzane tutaj — laduje je supervisor przy boocie i
// zostaja rezydentne (zachowanie desktop/serwer). Residency dotyczy wylacznie
// unpinned (domyslne na mobile, opt-in gdzie indziej).
// =============================================================================

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::db::DbPool;
use crate::services::ports::PortAllocator;
use crate::services_repo::services::DeployMethod;
use crate::services_repo::{models, services};

/// Jeden rezydentny unpinned model.
struct Resident {
    /// Kebab kategoria serwisu ("llm"/"stt"/"tts") — wybiera manager do unload.
    category: String,
    /// Klucz silnika w managerze TTS / etykieta (np. "sherpa-onnx").
    engine_id: String,
    /// Ostatnie uzycie — baza dla idle-unload.
    last_used: Instant,
}

pub struct ModelResidency {
    db: DbPool,
    ports: Arc<PortAllocator>,
    /// Szyfr `settings` — `respawn()` wymaga go do rozwiazania HF_TOKEN. Residency
    /// laduje tylko embedded (lokalne, bez pobierania z HF), wiec w praktyce token
    /// jest tu None, ale `respawn()` ma jeden kontrakt dla wszystkich sciezek.
    settings_cipher: Arc<crate::crypto::SettingsCipher>,
    idle_timeout: Duration,
    /// Rezydentne unpinned modele: model_name -> Resident.
    resident: Mutex<HashMap<String, Resident>>,
    /// Serializuje caly cykl ensure_loaded (evict + load), zeby dwa rownolegle
    /// zadania nie wyladowywaly sie nawzajem w polowie ladowania.
    ensure_lock: Mutex<()>,
}

impl ModelResidency {
    pub fn new(
        db: DbPool,
        ports: Arc<PortAllocator>,
        settings_cipher: Arc<crate::crypto::SettingsCipher>,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            db,
            ports,
            settings_cipher,
            idle_timeout,
            resident: Mutex::new(HashMap::new()),
            ensure_lock: Mutex::new(()),
        }
    }

    /// Upewnia sie ze model jest w pamieci. Dla UNPINNED embedded: lazy-load z
    /// eviction (single-resident). Dla pinned / nie-embedded: no-op (zarzadza
    /// nimi supervisor). Wolane przez executor przed dispatch do Embedded.
    pub async fn ensure_loaded(&self, model_name: &str) -> anyhow::Result<()> {
        // Szybki path: juz rezydentny → odswiez last_used i wracaj.
        {
            let mut res = self.resident.lock().await;
            if let Some(info) = res.get_mut(model_name) {
                info.last_used = Instant::now();
                return Ok(());
            }
        }

        // Serializuj caly evict+load (jeden lazy-load naraz; single-resident).
        let _ensure = self.ensure_lock.lock().await;
        // Re-check pod ensure_lock (mogl zaladowac inny task w trakcie czekania).
        {
            let mut res = self.resident.lock().await;
            if let Some(info) = res.get_mut(model_name) {
                info.last_used = Instant::now();
                return Ok(());
            }
        }

        // Znajdz serwis hostujacy ten model.
        let svc = {
            let conn = self
                .db
                .read()
                .map_err(|e| anyhow::anyhow!("model_residency: db read: {e}"))?;
            let model = models::get_by_name(&conn, model_name)?
                .ok_or_else(|| anyhow::anyhow!("model_residency: nieznany model '{model_name}'"))?;
            services::get(&conn, model.service_id)?.ok_or_else(|| {
                anyhow::anyhow!(
                    "model_residency: brak serwisu {} dla '{model_name}'",
                    model.service_id
                )
            })?
        };

        // Residency dotyczy tylko embedded in-process. HTTP/Docker/QUIC zyja
        // jako procesy/kontenery — nimi nie zarzadzamy. Pinned embedded laduje
        // supervisor; jesli pinned nie jest zaladowany, dispatch zglosi blad
        // (zachowanie jak dotad) — nie ladujemy go tu, zeby nie dublowac.
        if svc.deploy_method != DeployMethod::NativeEmbedded || svc.pinned {
            return Ok(());
        }

        // Eviction category-aware (NIE blanket single-resident — to thrashowalo
        // streaming audio-chat: LLM streamuje gdy TTS syntezuje rownolegle, wiec
        // load TTS wypieral LLM w polowie generacji → przeladowania → OOM).
        // TTS (sherpa/kokoro, 60-300 MB) WSPOLISTNIEJE z LLM: load TTS nie wypiera
        // niczego, TTS nigdy nie jest wypierany. Tylko DUZE modele (llm/stt, np.
        // whisper-large ~1GB) wypieraja sie nawzajem przed zaladowaniem duzego.
        self.evict_for(&svc.category, model_name).await;

        // Load przez reuse deploy::respawn (download+load+register z configu).
        // EMBEDDED_LOAD_GATE wewnatrz prepare_embedded_* serializuje sam load.
        crate::services::deploy::respawn(
            &svc.engine_id,
            svc.deploy_method,
            &svc.config_json,
            self.ports.clone(),
            &self.db,
            &self.settings_cipher,
            svc.runtime_port,
        )
        .await
        .map_err(|e| anyhow::anyhow!("lazy-load '{model_name}': {e}"))?;

        self.resident.lock().await.insert(
            model_name.to_string(),
            Resident {
                category: svc.category.clone(),
                engine_id: svc.engine_id.clone(),
                last_used: Instant::now(),
            },
        );
        info!(
            "model_residency: lazy-loaded '{}' ({} / {})",
            model_name, svc.category, svc.engine_id
        );
        Ok(())
    }

    /// Eviction category-aware przed zaladowaniem `keep_model` (kategoria
    /// `new_category`). TTS male → load TTS nic nie wypiera; TTS nigdy nie
    /// wypierany (wspolistnieje z LLM dla streaming audio-chat). DUZE (llm/stt)
    /// wypieraja inne DUZE (poza `keep_model` i poza TTS).
    async fn evict_for(&self, new_category: &str, keep_model: &str) {
        if new_category == "tts" {
            return;
        }
        let to_evict: Vec<(String, Resident)> = {
            let mut res = self.resident.lock().await;
            let names: Vec<String> = res
                .iter()
                .filter(|(name, info)| name.as_str() != keep_model && info.category != "tts")
                .map(|(name, _)| name.clone())
                .collect();
            names
                .into_iter()
                .filter_map(|n| res.remove(&n).map(|i| (n, i)))
                .collect()
        };
        for (name, info) in to_evict {
            unload_engine(&info.category, &info.engine_id).await;
            info!(
                "model_residency: evicted '{}' (load {})",
                name, new_category
            );
        }
    }

    /// Wyladowuje wszystkie rezydentne unpinned modele.
    async fn evict_all(&self) {
        let drained: Vec<(String, Resident)> = {
            let mut res = self.resident.lock().await;
            res.drain().collect()
        };
        for (name, info) in drained {
            unload_engine(&info.category, &info.engine_id).await;
            info!("model_residency: evicted '{}'", name);
        }
    }

    /// Memory guard: wyladuj wszystko (ostrzezenie o niskiej pamieci).
    pub async fn unload_all(&self) {
        self.evict_all().await;
    }

    /// Startuje watek idle-unload. Co 15s sprawdza last_used; zwalnia modele
    /// bezczynne dluzej niz `idle_timeout`.
    pub fn spawn_idle_unloader(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            tick.tick().await; // pomin natychmiastowy pierwszy tick
            loop {
                tick.tick().await;
                let now = Instant::now();
                let stale: Vec<(String, Resident)> = {
                    let mut res = self.resident.lock().await;
                    let names: Vec<String> = res
                        .iter()
                        .filter(|(_, i)| now.duration_since(i.last_used) > self.idle_timeout)
                        .map(|(n, _)| n.clone())
                        .collect();
                    names
                        .into_iter()
                        .filter_map(|n| res.remove(&n).map(|i| (n, i)))
                        .collect()
                };
                for (name, info) in stale {
                    unload_engine(&info.category, &info.engine_id).await;
                    info!("model_residency: idle-unloaded '{}'", name);
                }
            }
        });
    }
}

// =============================================================================
// Memory guard hook — globalny dostep dla reaktywnego unloadu (iOS/Android
// memory warning wolany z natywnego watku, poza runtime tokio).
// =============================================================================

use std::sync::OnceLock;

static GLOBAL_RESIDENCY: OnceLock<Arc<ModelResidency>> = OnceLock::new();
static RUNTIME_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();

/// Rejestruje globalna instancje residency + handle runtime (wolane raz w
/// runtime startup). Handle pozwala `trigger_memory_unload` spawnowac async
/// unload z DOWOLNEGO watku (np. iOS main thread przy didReceiveMemoryWarning,
/// gdzie `Handle::try_current()` nie dziala).
pub fn register_global(residency: Arc<ModelResidency>, handle: tokio::runtime::Handle) {
    let _ = GLOBAL_RESIDENCY.set(residency);
    let _ = RUNTIME_HANDLE.set(handle);
}

/// Memory guard: wyladowuje WSZYSTKIE unpinned modele z pamieci. Bezpieczne do
/// wolania z dowolnego watku (spawn na zapisanym handlu runtime). No-op gdy
/// residency nie zarejestrowane (desktop bez mobile lifecycle / testy).
pub fn trigger_memory_unload() {
    if let (Some(res), Some(handle)) = (GLOBAL_RESIDENCY.get(), RUNTIME_HANDLE.get()) {
        let res = res.clone();
        handle.spawn(async move {
            res.unload_all().await;
            warn!("model_residency: memory guard — wyladowano unpinned modele");
        });
    } else {
        warn!("model_residency: memory guard wywolany ale residency nie zarejestrowane");
    }
}

/// Wyladowuje silnik wlasciwego managera wg kategorii serwisu.
async fn unload_engine(category: &str, engine_id: &str) {
    match category {
        "llm" => {
            if let Err(e) = crate::inference::shared_inference_manager()
                .write()
                .await
                .unload_model()
                .await
            {
                warn!("model_residency: unload LLM '{}' failed: {}", engine_id, e);
            }
        }
        "stt" => {
            if let Err(e) = crate::stt::shared_stt_manager()
                .write()
                .await
                .unload_model()
                .await
            {
                warn!("model_residency: unload STT '{}' failed: {}", engine_id, e);
            }
        }
        "tts" => {
            crate::tts::shared_tts_manager()
                .write()
                .await
                .unregister(engine_id);
        }
        other => {
            warn!(
                "model_residency: nieznana kategoria '{}' przy unload",
                other
            );
        }
    }
}
