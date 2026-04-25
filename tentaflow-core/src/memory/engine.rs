// =============================================================================
// Plik: memory/engine.rs
// Opis: Trait LoadableEngine — wspolny interface dla wszystkich silnikow
//       AI (embedded, python-bundle, docker). MemoryGuard wola te metody
//       gdy decyduje o ladowaniu / unloadingu wg budzetu pamieci.
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait LoadableEngine: Send + Sync {
    /// Identyfikator silnika ("vllm-metal", "mlx", "llama-cpp", ...).
    fn engine_id(&self) -> &str;

    /// Service name w DB (tentaflow-vllm-metal-2izlb).
    fn service_name(&self) -> &str;

    /// Szacowane VRAM w MB po zaladowaniu modelu. Uzywane do decyzji
    /// czy starczy budzetu lub kogo wyrzucic.
    fn vram_estimated_mb(&self) -> u64;

    /// Aktualnie zaladowany w GPU/RAM.
    fn is_loaded(&self) -> bool;

    /// Ladowanie modelu / uruchomienie procesu. Idempotentne — wywolanie
    /// na zaladowanym silniku zwraca Ok bez akcji.
    async fn ensure_loaded(&self) -> Result<()>;

    /// Wyladowanie modelu / zatrzymanie procesu. Idempotentne. Po unload
    /// `is_loaded()` powinno zwracac false. Pinned services nie powinny
    /// byc unloadowane — MemoryGuard pomija je przy eviction.
    async fn unload(&self) -> Result<()>;
}
