// =============================================================================
// Plik: flow_engine/dispatchers/tts_cleaning.rs
// Opis: TtsCleaningStore — narrow trait dla cleaning tekstu przed TTS.
//       Implementacja opakowuje `tts::clean_cache::clean` (regex+cache+TTL
//       siedzą w impl, adapter widzi tylko clean(text) -> text).
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait TtsCleaningStore: Send + Sync {
    async fn clean(&self, text: &str) -> Result<String>;
}
