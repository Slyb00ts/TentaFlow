// =============================================================================
// Plik: memory/state.rs
// Opis: Stan serwisu w MemoryGuard — czy proces zyje, kiedy ostatnio go
//       uzywano, czy jest pinned (zawsze warm), czy paused (skip startupu).
// =============================================================================

use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadState {
    /// Proces nie istnieje (lub embedded model nie zaladowany).
    Cold,
    /// Trwa ladowanie/uruchamianie.
    Loading,
    /// Aktywny, gotowy do dispatch.
    Warm,
    /// Aktywny ale ostatnie uzycie > N sek temu (kandydat do eviction).
    Idle,
}

#[derive(Clone, Debug)]
pub struct ServiceMemState {
    pub service_name: String,
    pub state: LoadState,
    /// Ostatni request do tego serwisu — bazowa heurystyka LRU.
    pub last_used: Instant,
    /// Pinned: nigdy nie evict, zawsze ladowany przy starcie tentaflow.
    /// Wartosc z DB `services.pinned`.
    pub pinned: bool,
    /// Paused: nie startuje przy autostart, request tez ignorowany.
    /// Wartosc z DB `services.paused`.
    pub paused: bool,
    /// Szacowany rozmiar modelu w VRAM/RAM po zaladowaniu.
    /// Ustawiany przy register (z model_preset metadata albo pomiar po-load).
    pub vram_estimated_mb: u64,
}

impl ServiceMemState {
    pub fn new(service_name: String, vram_estimated_mb: u64) -> Self {
        Self {
            service_name,
            state: LoadState::Cold,
            last_used: Instant::now(),
            pinned: false,
            paused: false,
            vram_estimated_mb,
        }
    }

    pub fn is_loaded(&self) -> bool {
        matches!(self.state, LoadState::Warm | LoadState::Idle)
    }

    pub fn touch(&mut self) {
        self.last_used = Instant::now();
        if self.state == LoadState::Idle {
            self.state = LoadState::Warm;
        }
    }
}
