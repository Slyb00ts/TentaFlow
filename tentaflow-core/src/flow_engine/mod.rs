// =============================================================================
// Plik: flow_engine/mod.rs
// Opis: Modul Flow Engine - interpreter DAG dla konfigurowalnych przeplywow
//       przetwarzania requestow AI. Parsuje definicje flow z bazy danych
//       i wykonuje je krok po kroku.
// =============================================================================

pub mod adapters;
pub mod cache;
pub mod converter;
pub mod dispatcher;
pub mod executor_async;
pub mod resolver;
pub mod types;
pub mod validation;
