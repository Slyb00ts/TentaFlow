// =============================================================================
// Plik: memory/state.rs
// Opis: Stan serwisu w MemoryGuard — czy proces zyje, kiedy ostatnio go
//       uzywano, czy jest pinned (zawsze warm), czy paused (skip startupu).
// =============================================================================

use std::time::Instant;

/// Przypisanie GPU dla serwisu — krytyczne dla MemoryGuard zeby decyzja
/// eviction byla per-karta, nie sumarycznie. User w deploy wizardzie mowi
/// "uzyj GPU 0" lub "wszystkie", przy multi-GPU rig (np. 2x RTX 4090) jest
/// to konieczne — inaczej guard widzi "wolne 28 GB sumarycznie" gdy karta 0
/// ma 4 GB wolne a karta 1 ma 24 GB, i pakuje 30 GB model na karte 0 → OOM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuAffinity {
    /// Single GPU. Indeks z kolejnosci wgpu (== CUDA_VISIBLE_DEVICES idx).
    Single(usize),
    /// Multi-GPU tensor parallelism — model rozkladany na N kart. Pamiec
    /// szacowana na karte = vram_estimated_mb / indices.len().
    Multi(Vec<usize>),
    /// Wszystkie dostepne GPU (np. vllm tensor-parallel-size auto).
    All,
    /// CPU only — embeddings male, niektore TTS. Nie wplywa na VRAM.
    Cpu,
}

impl GpuAffinity {
    /// Czy serwis powinien byc liczony do uzycia danej karty.
    pub fn covers(&self, gpu_idx: usize, total_gpus: usize) -> bool {
        match self {
            GpuAffinity::Single(i) => *i == gpu_idx,
            GpuAffinity::Multi(v) => v.contains(&gpu_idx),
            GpuAffinity::All => gpu_idx < total_gpus,
            GpuAffinity::Cpu => false,
        }
    }

    /// Liczba kart na ktore VRAM jest dzielony. Cpu zwraca 0.
    pub fn gpu_count(&self, total_gpus: usize) -> usize {
        match self {
            GpuAffinity::Single(_) => 1,
            GpuAffinity::Multi(v) => v.len(),
            GpuAffinity::All => total_gpus,
            GpuAffinity::Cpu => 0,
        }
    }

    /// Lista konkretnych GPU indices (rozwija All do realnej listy).
    pub fn resolve_indices(&self, total_gpus: usize) -> Vec<usize> {
        match self {
            GpuAffinity::Single(i) => vec![*i],
            GpuAffinity::Multi(v) => v.clone(),
            GpuAffinity::All => (0..total_gpus).collect(),
            GpuAffinity::Cpu => Vec::new(),
        }
    }
}

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
    /// Szacowany rozmiar modelu w VRAM/RAM po zaladowaniu (sumarycznie po
    /// wszystkich kartach na ktorych serwis dziala).
    /// Ustawiany przy register (z model_preset metadata albo pomiar po-load).
    pub vram_estimated_mb: u64,
    /// GPU affinity — na ktorej karcie/kartach serwis ma sie zaladowac.
    /// MemoryGuard sprawdza budzet PER karta tylko dla tych z affinity,
    /// reszta gpu nie jest brana pod uwage.
    pub gpu_affinity: GpuAffinity,
}

impl ServiceMemState {
    /// VRAM zuzywane przez ten serwis na pojedynczej karcie. Dla single GPU
    /// = pelne vram_estimated_mb. Dla tensor-parallel multi-GPU = rownomiernie
    /// podzielone (heurystyka — niektore frameworki niesymetrycznie pakuja
    /// embeddings na rank 0, ale dla decyzji eviction wystarczajace).
    pub fn vram_per_gpu_mb(&self, total_gpus: usize) -> u64 {
        let n = self.gpu_affinity.gpu_count(total_gpus).max(1);
        self.vram_estimated_mb / n as u64
    }
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
            gpu_affinity: GpuAffinity::All,
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
