// =============================================================================
// Plik: memory/guard.rs
// Opis: MemoryGuard — globalny menedzer pamieci VRAM/RAM dla wszystkich
//       silnikow zarejestrowanych w systemie. Algorytm:
//         1) Request → ensure_loaded(service)
//         2) Jesli warm: touch last_used, return.
//         3) Jesli cold: sprawdz budzet. Wystarczy → load. Brak →
//            znajdz warm bez pinned/paused o najstarszym last_used,
//            unload, ponow check. Powtarzaj az starczy lub fail.
//         4) Pinned services nigdy nie sa kandydatami do eviction.
// =============================================================================

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use parking_lot::RwLock;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::engine::LoadableEngine;
use super::state::{GpuAffinity, LoadState, ServiceMemState};

static GLOBAL: OnceLock<Arc<MemoryGuard>> = OnceLock::new();

pub fn global() -> Arc<MemoryGuard> {
    GLOBAL
        .get_or_init(|| Arc::new(MemoryGuard::new(default_vram_budget_mb())))
        .clone()
}

fn default_vram_budget_mb() -> u64 {
    std::env::var("TENTAFLOW_VRAM_BUDGET_MB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

struct Entry {
    state: RwLock<ServiceMemState>,
    engine: Arc<dyn LoadableEngine>,
    /// Serializuje load/unload tego serwisu — zapobiega race gdy 2 requesty
    /// jednoczesnie probuja ensure_loaded.
    op_lock: Mutex<()>,
}

pub struct MemoryGuard {
    entries: RwLock<HashMap<String, Arc<Entry>>>,
    /// 0 = unlimited (zawsze ladujemy bez eviction). Wartosc inna niz 0
    /// wlacza eviction gdy sumaryczne warm vram przekroczy budzet.
    total_vram_budget_mb: RwLock<u64>,
    /// Globalny mutex dla decyzji eviction — zapobiega ze 2 requesty
    /// jednoczesnie wybiora ten sam ofiarny service.
    eviction_lock: Mutex<()>,
}

impl MemoryGuard {
    pub fn new(total_vram_budget_mb: u64) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            total_vram_budget_mb: RwLock::new(total_vram_budget_mb),
            eviction_lock: Mutex::new(()),
        }
    }

    pub fn set_budget_mb(&self, budget_mb: u64) {
        *self.total_vram_budget_mb.write() = budget_mb;
    }

    pub fn budget_mb(&self) -> u64 {
        *self.total_vram_budget_mb.read()
    }

    pub fn register(
        &self,
        service_name: String,
        engine: Arc<dyn LoadableEngine>,
        vram_estimated_mb: u64,
        pinned: bool,
        paused: bool,
        gpu_affinity: GpuAffinity,
    ) {
        let mut state = ServiceMemState::new(service_name.clone(), vram_estimated_mb);
        state.pinned = pinned;
        state.paused = paused;
        state.gpu_affinity = gpu_affinity.clone();
        if engine.is_loaded() {
            state.state = LoadState::Warm;
        }
        let entry = Arc::new(Entry {
            state: RwLock::new(state),
            engine,
            op_lock: Mutex::new(()),
        });
        self.entries.write().insert(service_name.clone(), entry);
        info!(
            service = %service_name, vram_mb = vram_estimated_mb,
            pinned, paused, affinity = ?gpu_affinity,
            "MemoryGuard: zarejestrowano serwis"
        );
    }

    pub fn unregister(&self, service_name: &str) {
        self.entries.write().remove(service_name);
    }

    pub fn snapshot(&self) -> Vec<ServiceMemState> {
        self.entries
            .read()
            .values()
            .map(|e| e.state.read().clone())
            .collect()
    }

    pub fn set_pinned(&self, service_name: &str, pinned: bool) -> Result<()> {
        let entries = self.entries.read();
        let e = entries
            .get(service_name)
            .ok_or_else(|| anyhow!("service '{}' nie zarejestrowany w guard", service_name))?;
        e.state.write().pinned = pinned;
        Ok(())
    }

    pub fn set_paused(&self, service_name: &str, paused: bool) -> Result<()> {
        let entries = self.entries.read();
        let e = entries
            .get(service_name)
            .ok_or_else(|| anyhow!("service '{}' nie zarejestrowany w guard", service_name))?;
        e.state.write().paused = paused;
        Ok(())
    }

    fn get_entry(&self, service_name: &str) -> Option<Arc<Entry>> {
        self.entries.read().get(service_name).cloned()
    }

    pub fn is_paused(&self, service_name: &str) -> bool {
        self.get_entry(service_name)
            .map(|e| e.state.read().paused)
            .unwrap_or(false)
    }

    /// Sumaryczne vram_estimated_mb dla wszystkich warm/idle services.
    fn total_warm_vram_mb(&self) -> u64 {
        self.entries
            .read()
            .values()
            .map(|e| {
                let s = e.state.read();
                if s.is_loaded() {
                    s.vram_estimated_mb
                } else {
                    0
                }
            })
            .sum()
    }

    /// VRAM zajety przez warm services na konkretnym GPU. Sumuje vram_per_gpu
    /// dla services ktorych affinity covers ten gpu_idx.
    fn warm_vram_on_gpu(&self, gpu_idx: usize, total_gpus: usize) -> u64 {
        self.entries
            .read()
            .values()
            .map(|e| {
                let s = e.state.read();
                if s.is_loaded() && s.gpu_affinity.covers(gpu_idx, total_gpus) {
                    s.vram_per_gpu_mb(total_gpus)
                } else {
                    0
                }
            })
            .sum()
    }

    /// Glowna metoda: wywolywana przed kazdym dispatch. Gdy service zaladowany
    /// — touch last_used + return. Gdy nie — load z eviction jesli trzeba.
    pub async fn ensure_loaded(&self, service_name: &str) -> Result<()> {
        let entry = self.get_entry(service_name).ok_or_else(|| {
            anyhow!(
                "service '{}' nie zarejestrowany w MemoryGuard",
                service_name
            )
        })?;

        // Paused — odrzucamy z jasnym bledem, nie ladujemy.
        if entry.state.read().paused {
            return Err(anyhow!(
                "service '{}' jest paused — uruchom go najpierw w GUI",
                service_name
            ));
        }

        // Fast path: juz warm.
        {
            let mut s = entry.state.write();
            if s.is_loaded() {
                s.touch();
                return Ok(());
            }
        }

        // Slow path — serializuj load tego service (op_lock).
        let _op = entry.op_lock.lock().await;

        // Re-check po nabyciu locka — moze inny task juz zaladowal.
        {
            let mut s = entry.state.write();
            if s.is_loaded() {
                s.touch();
                return Ok(());
            }
            s.state = LoadState::Loading;
        }

        // Per-GPU decyzja eviction. Sprawdzamy budzet TYLKO dla kart z
        // affinity tego serwisu — multi-GPU rig moze miec wolne VRAM na karcie 1
        // a pelne na karcie 0, a serwis pinowany na karcie 0 nie pomoze
        // sumarycznemu rachunkowi.
        let (affinity, needed_total) = {
            let s = entry.state.read();
            (s.gpu_affinity.clone(), s.vram_estimated_mb)
        };

        // Cpu — pomijamy VRAM check w ogole.
        if !matches!(affinity, GpuAffinity::Cpu) {
            let snapshots = crate::mesh::node_info_collector::vram_snapshot_per_gpu();
            let total_gpus = snapshots.len();
            let target_indices = affinity.resolve_indices(total_gpus);

            // Per karta: sprawdz needed_per_gpu vs free_per_gpu. Jesli ktorakolwiek
            // karta z affinity nie miesci, eviction tylko z tej karty.
            let needed_per_gpu = needed_total / target_indices.len().max(1) as u64;

            for &idx in &target_indices {
                let snapshot = match snapshots.get(idx) {
                    Some(s) => s,
                    None => continue, // brak GPU o tym indeksie — skip
                };
                let warm_on_gpu = self.warm_vram_on_gpu(idx, total_gpus);
                let conservative_used = snapshot.used_mb.max(warm_on_gpu);
                let limit = snapshot.total_mb;

                if limit == 0 {
                    continue; // brak danych o karcie (ioreg nie zwrocil) — skip
                }

                if conservative_used + needed_per_gpu > limit {
                    let _eviction = self.eviction_lock.lock().await;
                    let to_free = (conservative_used + needed_per_gpu).saturating_sub(limit);
                    tracing::info!(
                        service = %service_name, gpu = idx,
                        needed_mb = needed_per_gpu, used_mb = conservative_used,
                        total_mb = limit, gpu_name = %snapshot.name,
                        "MemoryGuard: eviction needed na GPU {}", idx
                    );
                    self.evict_at_least_on_gpu(to_free, idx, total_gpus, service_name)
                        .await?;
                } else {
                    tracing::debug!(
                        service = %service_name, gpu = idx,
                        needed_mb = needed_per_gpu, free_mb = snapshot.free_mb,
                        "MemoryGuard: GPU {} ma miejsce, brak eviction", idx
                    );
                }
            }
        }

        // Override TENTAFLOW_VRAM_BUDGET_MB — globalny hard cap (nadrzedny nad
        // per-GPU). Uzyteczny na shared hostach gdzie nie chcemy wziac calego
        // GPU. Sprawdzane sumarycznie po affinity.
        let budget_override = self.budget_mb();
        if budget_override > 0 {
            let warm_total = self.total_warm_vram_mb();
            if warm_total + needed_total > budget_override {
                let _eviction = self.eviction_lock.lock().await;
                let to_free = (warm_total + needed_total).saturating_sub(budget_override);
                tracing::info!(
                    service = %service_name,
                    "MemoryGuard: override hard cap exceeded ({} + {} > {}), evicting",
                    warm_total, needed_total, budget_override
                );
                self.evict_at_least(to_free, service_name).await?;
            }
        }

        // Properna load.
        let load_result = entry.engine.ensure_loaded().await;
        match load_result {
            Ok(()) => {
                let mut s = entry.state.write();
                s.state = LoadState::Warm;
                s.touch();
                info!(
                    service = %service_name, vram_mb = s.vram_estimated_mb,
                    "MemoryGuard: zaladowano serwis"
                );
                Ok(())
            }
            Err(e) => {
                let mut s = entry.state.write();
                s.state = LoadState::Cold;
                Err(e).with_context(|| format!("ensure_loaded({})", service_name))
            }
        }
    }

    /// Wymusza unload jesli dozwolone (nie pinned, nie paused). Bez wzgledu
    /// na last_used — uzywane przy manualnym Pause z GUI.
    pub async fn force_unload(&self, service_name: &str) -> Result<()> {
        let entry = self
            .get_entry(service_name)
            .ok_or_else(|| anyhow!("service '{}' nie zarejestrowany", service_name))?;
        let _op = entry.op_lock.lock().await;
        if !entry.state.read().is_loaded() {
            return Ok(());
        }
        entry.engine.unload().await?;
        let mut s = entry.state.write();
        s.state = LoadState::Cold;
        Ok(())
    }

    /// Eviction NA KONKRETNEJ KARCIE — kandydaci to warm services ktorych
    /// affinity covers `gpu_idx`. Zwolniony VRAM liczony jako vram_per_gpu na
    /// tej karcie (multi-GPU service uwalnia tylko swoja "doze" z tej karty).
    async fn evict_at_least_on_gpu(
        &self,
        needed_mb: u64,
        gpu_idx: usize,
        total_gpus: usize,
        requesting: &str,
    ) -> Result<u64> {
        if needed_mb == 0 {
            return Ok(0);
        }
        let mut candidates: Vec<(String, Instant, u64, Arc<Entry>)> = self
            .entries
            .read()
            .iter()
            .filter_map(|(name, entry)| {
                let s = entry.state.read();
                if s.pinned
                    || s.paused
                    || !s.is_loaded()
                    || name == requesting
                    || !s.gpu_affinity.covers(gpu_idx, total_gpus)
                {
                    None
                } else {
                    let vram_on_gpu = s.vram_per_gpu_mb(total_gpus);
                    Some((name.clone(), s.last_used, vram_on_gpu, entry.clone()))
                }
            })
            .collect();
        candidates.sort_by_key(|(_, t, _, _)| *t);

        let mut freed = 0u64;
        for (name, _, vram_on_gpu, entry) in candidates {
            if freed >= needed_mb {
                break;
            }
            debug!(
                service = %name, vram_mb = vram_on_gpu, gpu = gpu_idx,
                "MemoryGuard: eviction kandydat na GPU {} — unload", gpu_idx
            );
            // Unload zwalnia VRAM ze WSZYSTKICH kart na ktorych ten serwis byl,
            // nie tylko tej. Akceptujemy — wzywany przez user jako koszt eviction.
            match entry.engine.unload().await {
                Ok(()) => {
                    entry.state.write().state = LoadState::Cold;
                    freed = freed.saturating_add(vram_on_gpu);
                    info!(
                        service = %name, freed_mb = vram_on_gpu, gpu = gpu_idx,
                        "MemoryGuard: zwolniono na GPU {} przez eviction", gpu_idx
                    );
                }
                Err(e) => warn!(
                    service = %name, error = %e,
                    "MemoryGuard: unload nieudany podczas eviction — pomijam"
                ),
            }
        }

        if freed < needed_mb {
            warn!(
                requested_mb = needed_mb,
                freed_mb = freed,
                gpu = gpu_idx,
                requesting,
                "MemoryGuard: nie udalo sie zwolnic wystarczajaco na GPU {}",
                gpu_idx
            );
        }
        Ok(freed)
    }

    /// Wybiera kandydatow do eviction wg LRU, pomija pinned i requesting.
    /// Unloaduje az suma freed >= needed_mb. Zwraca sume zwolnionego.
    /// Sumaryczna wersja — uzywana TYLKO dla TENTAFLOW_VRAM_BUDGET_MB hard cap
    /// (per-GPU eviction obsluguje normalny przypadek).
    async fn evict_at_least(&self, needed_mb: u64, requesting: &str) -> Result<u64> {
        if needed_mb == 0 {
            return Ok(0);
        }

        // Zbierz kandydatow: warm/idle, nie pinned, nie paused, != requesting.
        let mut candidates: Vec<(String, Instant, u64, Arc<Entry>)> = self
            .entries
            .read()
            .iter()
            .filter_map(|(name, entry)| {
                let s = entry.state.read();
                if s.pinned || s.paused || !s.is_loaded() || name == requesting {
                    None
                } else {
                    Some((
                        name.clone(),
                        s.last_used,
                        s.vram_estimated_mb,
                        entry.clone(),
                    ))
                }
            })
            .collect();

        // LRU: najstarszy last_used jako pierwszy.
        candidates.sort_by_key(|(_, t, _, _)| *t);

        let mut freed = 0u64;
        for (name, _, vram, entry) in candidates {
            if freed >= needed_mb {
                break;
            }
            debug!(
                service = %name, vram_mb = vram,
                "MemoryGuard: eviction kandydat — unload"
            );
            let unload_res = entry.engine.unload().await;
            match unload_res {
                Ok(()) => {
                    entry.state.write().state = LoadState::Cold;
                    freed = freed.saturating_add(vram);
                    info!(
                        service = %name, freed_mb = vram,
                        "MemoryGuard: zwolniono pamiec przez eviction"
                    );
                }
                Err(e) => {
                    warn!(
                        service = %name, error = %e,
                        "MemoryGuard: unload nieudany podczas eviction — pomijam"
                    );
                }
            }
        }

        if freed < needed_mb {
            warn!(
                requested_mb = needed_mb,
                freed_mb = freed,
                requesting,
                "MemoryGuard: nie udalo sie zwolnic wystarczajaco — moze brakowac VRAM"
            );
        }
        Ok(freed)
    }
}
