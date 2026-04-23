// =============================================================================
// Plik: flow_engine/adapters/mod.rs
// Opis: Modul adapterow wezlow Flow Engine - most miedzy DAG a serwisami
//       routera. Kazdy adapter implementuje trait NodeAdapter i deleguje
//       wykonanie do odpowiedniego backendu (LLM, RAG, STT, TTS itd.).
// =============================================================================

pub mod condition;
pub mod conversation_history;
pub mod embeddings;
pub mod llm;
pub mod memory;
pub mod output;
pub mod pii_filter;
pub mod rag;
pub mod session_context;
pub mod speaker_context;
pub mod stt;
pub mod trigger;
pub mod tts;
pub mod tts_clean;

use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::pin::Pin;

use crate::api::openai::types::ChatCompletionChunk;
use crate::flow_engine::types::FlowContext;

/// Strumien chunkow SSE zwracany przez streamujace adaptery. Konkretny typ
/// `ChatCompletionChunk` (nie generyczny JSON) bo S4b obsluguje tylko chat SSE —
/// dalsze typy output-portow dojda razem z ich konsumentami, nie na zapas.
pub type AdapterChunkStream =
    Pin<Box<dyn futures::Stream<Item = Result<ChatCompletionChunk>> + Send>>;

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

    /// Lista dostepnych portow wyjsciowych tego typu node'a. Default tylko "full".
    /// Adaptery streamujace (LLM, TTS) override'uja i dodaja "stream".
    /// Uzywamy &'static [&'static str] zeby nie alokowac — listy sa statyczne
    /// per adapter i walidacja wywoluje to przy kazdym save flow_json.
    fn supported_output_ports(&self) -> &'static [&'static str] {
        &["full"]
    }

    /// Lista dostepnych portow wejsciowych. Default tylko "in".
    fn supported_input_ports(&self) -> &'static [&'static str] {
        &["in"]
    }

    /// Wariant streamujacy. Default `None` = adapter nie wspiera streamingu
    /// i executor uzyje blocking `execute()`. Adaptery ktore deklaruja
    /// `from_port="stream"` w `supported_output_ports()` musza implementowac.
    fn execute_streaming(
        &self,
        _node_config: &Value,
        _ctx: &mut FlowContext,
    ) -> impl std::future::Future<Output = Option<Result<AdapterChunkStream>>> + Send {
        async { None }
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

    fn supported_output_ports(&self) -> &'static [&'static str];

    fn supported_input_ports(&self) -> &'static [&'static str];

    fn execute_streaming_dyn<'a>(
        &'a self,
        node_config: &'a Value,
        ctx: &'a mut FlowContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<Result<AdapterChunkStream>>> + Send + 'a>,
    >;
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

    fn supported_output_ports(&self) -> &'static [&'static str] {
        NodeAdapter::supported_output_ports(self)
    }

    fn supported_input_ports(&self) -> &'static [&'static str] {
        NodeAdapter::supported_input_ports(self)
    }

    fn execute_streaming_dyn<'a>(
        &'a self,
        node_config: &'a Value,
        ctx: &'a mut FlowContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<Result<AdapterChunkStream>>> + Send + 'a>,
    > {
        Box::pin(NodeAdapter::execute_streaming(self, node_config, ctx))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::types::FlowContext;
    use anyhow::Result;

    struct DefaultsAdapter;
    impl NodeAdapter for DefaultsAdapter {
        fn execute(
            &self,
            _c: &Value,
            _ctx: &mut FlowContext,
        ) -> impl std::future::Future<Output = Result<Value>> + Send {
            async { Ok(Value::Null) }
        }
        fn node_type(&self) -> &'static str {
            "defaults"
        }
    }

    struct StreamyAdapter;
    impl NodeAdapter for StreamyAdapter {
        fn execute(
            &self,
            _c: &Value,
            _ctx: &mut FlowContext,
        ) -> impl std::future::Future<Output = Result<Value>> + Send {
            async { Ok(Value::Null) }
        }
        fn node_type(&self) -> &'static str {
            "streamy"
        }
        fn supported_output_ports(&self) -> &'static [&'static str] {
            &["stream", "full"]
        }
    }

    #[test]
    fn default_ports_full_and_in() {
        let a = DefaultsAdapter;
        assert_eq!(NodeAdapter::supported_output_ports(&a), &["full"]);
        assert_eq!(NodeAdapter::supported_input_ports(&a), &["in"]);
    }

    #[tokio::test]
    async fn default_execute_streaming_returns_none() {
        let a = DefaultsAdapter;
        let mut ctx = FlowContext::default();
        let out = NodeAdapter::execute_streaming(&a, &Value::Null, &mut ctx).await;
        assert!(out.is_none(), "default must not pretend to stream");
    }

    #[tokio::test]
    async fn default_execute_streaming_through_dyn_returns_none() {
        let mut reg = AdapterRegistry::new();
        reg.register(StreamyAdapter);
        let dyn_adapter = reg.get("streamy").expect("adapter present");
        let mut ctx = FlowContext::default();
        let out = dyn_adapter
            .execute_streaming_dyn(&Value::Null, &mut ctx)
            .await;
        assert!(out.is_none());
    }

    #[test]
    fn override_ports_propagate_through_dyn() {
        let mut reg = AdapterRegistry::new();
        reg.register(StreamyAdapter);
        let dyn_adapter = reg.get("streamy").expect("adapter present");
        assert_eq!(dyn_adapter.supported_output_ports(), &["stream", "full"]);
        assert_eq!(dyn_adapter.supported_input_ports(), &["in"]);
    }
}
