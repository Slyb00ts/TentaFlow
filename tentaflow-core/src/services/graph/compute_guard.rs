// ===== Plik: services/graph/compute_guard.rs — cap współbieżności ciężkich obliczeń grafowych =====
//
// Ciężkie prymitywy grafowe (neighbors/pagerank/ppr) trzymają read-lock kolekcji
// i liczą — bez ograniczenia współbieżności pojedynczy addon mógłby odpalić N
// takich obliczeń równolegle i wysycić CPU. Ten moduł jest JEDNYM, WSPÓŁDZIELONYM
// mechanizmem capa: te same liczniki (globalny + per-addon) i ten sam RAII-guard
// obsługują OBIE ścieżki wejścia — host-fn `addon::host_functions::graph` ORAZ
// węzeł flow `flow_engine::node_adapters::graph_search`. Dzięki temu addon nie
// obchodzi kontroli DoS, wywołując ciężkie obliczenia przez flow zamiast host-fn:
// liczniki są te same.
//
// Acquire PRZED pracą, RAII guard zwalnia slot także przy panice/błędzie (Drop).
// Saturacja → fail-closed `GraphError::ComputeBusy`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use dashmap::DashMap;

use super::error::GraphError;

/// Maks. równoległych ciężkich obliczeń grafowych w CAŁYM procesie.
pub const MAX_GLOBAL_GRAPH_COMPUTE: usize = 8;

/// Maks. równoległych ciężkich obliczeń grafowych dla POJEDYNCZEGO addona.
pub const MAX_PER_ADDON_GRAPH_COMPUTE: usize = 2;

/// Globalny licznik in-flight ciężkich obliczeń grafowych.
static GLOBAL_GRAPH_COMPUTE: AtomicUsize = AtomicUsize::new(0);

/// Per-addon liczniki in-flight (`addon_id -> count`). Wpisy zostają (mała,
/// ograniczona liczbą zainstalowanych addonów mapa); licznik wraca do 0.
static PER_ADDON_GRAPH_COMPUTE: OnceLock<DashMap<String, AtomicUsize>> = OnceLock::new();

/// Leniwie inicjalizowana mapa per-addon liczników.
fn per_addon_compute() -> &'static DashMap<String, AtomicUsize> {
    PER_ADDON_GRAPH_COMPUTE.get_or_init(DashMap::new)
}

/// RAII-guard slotu obliczeń grafowych. `acquire` inkrementuje globalny i
/// per-addon licznik PRZED pracą; jeśli którykolwiek przekroczyłby cap, roluje
/// inkrement z powrotem i zwraca `Err(ComputeBusy)` (fail-closed). `Drop`
/// dekrementuje oba liczniki — slot wraca także przy panice/błędzie pracy.
pub struct GraphComputeGuard {
    addon_id: String,
}

impl GraphComputeGuard {
    /// Próbuje zająć slot globalny i per-addon. Kolejność: najpierw globalny
    /// (fetch_add + sprawdzenie capa, rollback jeśli za dużo), potem per-addon
    /// (to samo, z rollbackiem globalnego, gdy per-addon saturuje).
    pub fn acquire(addon_id: &str) -> Result<Self, GraphError> {
        let prev_global = GLOBAL_GRAPH_COMPUTE.fetch_add(1, Ordering::AcqRel);
        if prev_global >= MAX_GLOBAL_GRAPH_COMPUTE {
            GLOBAL_GRAPH_COMPUTE.fetch_sub(1, Ordering::AcqRel);
            return Err(GraphError::ComputeBusy {
                scope: "global",
                max: MAX_GLOBAL_GRAPH_COMPUTE,
            });
        }

        let counter = per_addon_compute()
            .entry(addon_id.to_string())
            .or_insert_with(|| AtomicUsize::new(0));
        let prev_addon = counter.fetch_add(1, Ordering::AcqRel);
        if prev_addon >= MAX_PER_ADDON_GRAPH_COMPUTE {
            counter.fetch_sub(1, Ordering::AcqRel);
            drop(counter);
            GLOBAL_GRAPH_COMPUTE.fetch_sub(1, Ordering::AcqRel);
            return Err(GraphError::ComputeBusy {
                scope: "per_addon",
                max: MAX_PER_ADDON_GRAPH_COMPUTE,
            });
        }
        drop(counter);

        Ok(GraphComputeGuard {
            addon_id: addon_id.to_string(),
        })
    }
}

impl Drop for GraphComputeGuard {
    fn drop(&mut self) {
        GLOBAL_GRAPH_COMPUTE.fetch_sub(1, Ordering::AcqRel);
        if let Some(c) = per_addon_compute().get(&self.addon_id) {
            c.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
