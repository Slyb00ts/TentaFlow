// =============================================================================
// Plik: flow_engine/dispatchers/prompts.rs
// Opis: PromptStore — narrow trait nad prompt_registry::SharedPromptRegistry.
//       Adapter używa tylko `get_prompt(key, locale)` — write side (CRUD)
//       żyje dalej w dashboard handlers, nie w flow path.
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait PromptStore: Send + Sync {
    /// Pobierz prompt po kluczu. `locale` (np. "pl", "en") — store decyduje
    /// fallback gdy brak wariantu w danym locale.
    async fn get_prompt(&self, key: &str, locale: Option<&str>) -> Result<Option<String>>;
}
