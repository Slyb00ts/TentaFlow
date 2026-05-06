// =============================================================================
// Plik: flow_engine/dispatchers_impl/tts_cleaning_impl.rs
// Opis: TtsCleaningStoreImpl — wrapper nad `tts::clean_cache::clean`. Cache
//       regex+TTL żyje w `clean_cache`; wrapper tylko przepuszcza tekst.
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

use crate::db::DbPool;
use crate::flow_engine::dispatchers::TtsCleaningStore;

pub struct TtsCleaningStoreImpl {
    db: DbPool,
}

impl TtsCleaningStoreImpl {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl TtsCleaningStore for TtsCleaningStoreImpl {
    async fn clean(&self, text: &str) -> Result<String> {
        let db = self.db.clone();
        let owned = text.to_string();
        let cleaned =
            tokio::task::spawn_blocking(move || crate::tts::clean_cache::clean(&owned, &db))
                .await?;
        Ok(cleaned)
    }
}
