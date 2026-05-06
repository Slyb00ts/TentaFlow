// =============================================================================
// Plik: flow_engine/dispatchers/memory.rs
// Opis: MemoryStore — narrow trait nad memory engine (dziś wołane jako
//       `find_quic_client_for_model("memory")` w adapters/memory.rs:34).
//       Pokrywa oba tryby istniejącego adaptera: "query" → recall, "store"
//       → store. Store decyduje czy to lokalny LLM-driven memory engine,
//       czy mesh.
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct MemoryQuery {
    pub session_id: Option<String>,
    pub person_id: Option<String>,
    pub query_text: String,
    pub top_k: u32,
}

#[derive(Debug, Clone)]
pub struct MemoryHit {
    pub content: String,
    pub score: f32,
    pub source_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryRecall {
    pub hits: Vec<MemoryHit>,
}

/// Wpis do zapisu w memory engine. `tags` służą do późniejszego filtrowania
/// recall (np. "person:alice", "topic:project-x").
#[derive(Debug, Clone)]
pub struct MemoryRecord {
    pub session_id: Option<String>,
    pub person_id: Option<String>,
    pub content: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MemoryStoreReceipt {
    pub stored: bool,
    pub record_id: Option<String>,
}

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn recall(&self, query: MemoryQuery) -> Result<MemoryRecall>;

    /// Zapis nowej obserwacji. Engine zwraca `record_id` jeśli dostarcza —
    /// niektóre LLM-driven engine'y zapisują asynchronicznie i odpowiadają
    /// `stored=true` bez ID.
    async fn store(&self, record: MemoryRecord) -> Result<MemoryStoreReceipt>;
}
