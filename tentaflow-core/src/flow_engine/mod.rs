// =============================================================================
// Plik: flow_engine/mod.rs
// Opis: Modul Flow Engine - interpreter DAG dla konfigurowalnych przeplywow
//       przetwarzania requestow AI. Parsuje definicje flow z bazy danych
//       i wykonuje je krok po kroku.
// =============================================================================

pub mod adapters;
pub mod blob_store;
pub mod cache;
pub mod cancel_on_drop;
pub mod converter;
pub mod dispatcher;
pub mod dispatchers;
pub mod dispatchers_impl;
pub mod envelope;
pub mod executor_async;
pub mod node_adapter;
pub mod node_adapters;
pub mod resolver;
pub mod types;
pub mod validation;
