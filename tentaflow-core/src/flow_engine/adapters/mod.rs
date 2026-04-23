// =============================================================================
// Plik: flow_engine/adapters/mod.rs
// Opis: Modul adapterow wezlow Flow Engine - most miedzy DAG a serwisami
//       routera. Kazdy adapter implementuje trait NodeAdapter i deleguje
//       wykonanie do odpowiedniego backendu (LLM, RAG, STT, TTS itd.).
// =============================================================================

pub mod conversation_history;
pub mod embeddings;
pub mod llm;
pub mod memory;
pub mod rag;
pub mod session_context;
pub mod speaker_context;
pub mod stt;
pub mod tts;

use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;

use crate::flow_engine::types::FlowContext;

/// Bazowy trait dla wszystkich adapterow wezlow.
/// Adaptery tlumacza konfiguracje wezla DAG na wywolanie prawdziwego serwisu.
pub trait NodeAdapter: Send + Sync {
    /// Wykonuje logike wezla i zwraca wynik jako JSON
    fn execute(
        &self,
        node_config: &Value,
        ctx: &mut FlowContext,
    ) -> impl std::future::Future<Output = Result<Value>> + Send;

    /// Nazwa typu wezla ktory ten adapter obsluguje (np. "llm", "rag")
    fn node_type(&self) -> &'static str;

    /// Czy adapter wspiera streaming (domyslnie nie)
    fn supports_streaming(&self) -> bool {
        false
    }
}

/// Rejestr adapterow - mapuje typ wezla na konkretny adapter.
/// Silnik flow uzywa rejestru do odnalezienia adaptera dla danego typu wezla.
pub struct AdapterRegistry {
    adapters: HashMap<String, Box<dyn NodeAdapterDyn>>,
}

/// Wersja trait z dynamicznym dispatchem (object-safe).
/// Natywne async fn w trait nie sa object-safe, wiec potrzebujemy wrappera.
pub trait NodeAdapterDyn: Send + Sync {
    fn execute_dyn<'a>(
        &'a self,
        node_config: &'a Value,
        ctx: &'a mut FlowContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + 'a>>;

    fn node_type(&self) -> &'static str;

    fn supports_streaming(&self) -> bool;
}

/// Automatyczna implementacja NodeAdapterDyn dla kazdego typu
/// ktory implementuje NodeAdapter
impl<T: NodeAdapter> NodeAdapterDyn for T {
    fn execute_dyn<'a>(
        &'a self,
        node_config: &'a Value,
        ctx: &'a mut FlowContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(self.execute(node_config, ctx))
    }

    fn node_type(&self) -> &'static str {
        NodeAdapter::node_type(self)
    }

    fn supports_streaming(&self) -> bool {
        NodeAdapter::supports_streaming(self)
    }
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
        }
    }

    /// Rejestruje adapter w rejestrze
    pub fn register<A: NodeAdapter + 'static>(&mut self, adapter: A) {
        let node_type = adapter.node_type().to_string();
        self.adapters.insert(node_type, Box::new(adapter));
    }

    /// Pobiera adapter dla danego typu wezla
    pub fn get(&self, node_type: &str) -> Option<&dyn NodeAdapterDyn> {
        self.adapters.get(node_type).map(|a| a.as_ref())
    }

    /// Sprawdza czy adapter dla danego typu jest zarejestrowany
    pub fn has(&self, node_type: &str) -> bool {
        self.adapters.contains_key(node_type)
    }

    /// Zwraca liste zarejestrowanych typow wezlow
    pub fn registered_types(&self) -> Vec<&str> {
        self.adapters.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}
