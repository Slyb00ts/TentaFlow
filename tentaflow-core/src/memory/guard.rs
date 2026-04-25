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
use super::state::{LoadState, ServiceMemState};

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
    ) {
        let mut state = ServiceMemState::new(service_name.clone(), vram_estimated_mb);
        state.pinned = pinned;
        state.paused = paused;
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
            pinned, paused, "MemoryGuard: zarejestrowano serwis"
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

    /// Glowna metoda: wywolywana przed kazdym dispatch. Gdy service zaladowany
    /// — touch last_used + return. Gdy nie — load z eviction jesli trzeba.
    pub async fn ensure_loaded(&self, service_name: &str) -> Result<()> {
        let entry = self
            .get_entry(service_name)
            .ok_or_else(|| anyhow!("service '{}' nie zarejestrowany w MemoryGuard", service_name))?;

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

        // Eviction wg REAL VRAM z node_info_collector — sumuje aktualnie zajety
        // VRAM po wszystkich procesach systemu (nasz tentaflow + ext silniki +
        // inne aplikacje), nie tylko serwisy zarejestrowane w guardzie.
        // Cache 2s wiec wywolanie tanie. Override przez TENTAFLOW_VRAM_BUDGET_MB
        // (gdy user chce sztywno ograniczyc — uzyteczne na shared host).
        let needed = entry.state.read().vram_estimated_mb;
        let (total_real, used_real, free_real) =
            crate::mesh::node_info_collector::vram_snapshot_local();
        let budget_override = self.budget_mb();

        // Wybieramy efektywny limit: override (jesli ustawiony) albo realny
        // total. Free = limit - max(used_real, sum(warm guard estimates))
        // — bezpieczniej brac wieksza wartosc, zeby nie przepelnic.
        let effective_limit = if budget_override > 0 {
            budget_override
        } else {
            total_real
        };
        let warm_guard_estimate = self.total_warm_vram_mb();
        let conservative_used = used_real.max(warm_guard_estimate);

        if effective_limit > 0 && conservative_used + needed > effective_limit {
            let _eviction = self.eviction_lock.lock().await;
            let to_free = (conservative_used + needed).saturating_sub(effective_limit);
            tracing::info!(
                service = %service_name, needed_mb = needed,
                used_mb = conservative_used, total_mb = effective_limit,
                free_mb = effective_limit.saturating_sub(conservative_used),
                "MemoryGuard: eviction needed (real VRAM={}, used={}, override={})",
                total_real, used_real, budget_override
            );
            self.evict_at_least(to_free, service_name).await?;
        } else {
            tracing::debug!(
                service = %service_name, needed_mb = needed,
                free_mb = free_real,
                "MemoryGuard: budget OK, ladujemy bez eviction"
            );
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

    /// Wybiera kandydatow do eviction wg LRU, pomija pinned i requesting.
    /// Unloaduje az suma freed >= needed_mb. Zwraca sume zwolnionego.
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
                    Some((name.clone(), s.last_used, s.vram_estimated_mb, entry.clone()))
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
                requested_mb = needed_mb, freed_mb = freed, requesting,
                "MemoryGuard: nie udalo sie zwolnic wystarczajaco — moze brakowac VRAM"
            );
        }
        Ok(freed)
    }
}
