// ============================================================================
// TENTAFLOW CLIENT - Natywna biblioteka Rust dla P/Invoke z .NET
// ============================================================================
//
// CEL:
// Ta biblioteka zapewnia FFI (Foreign Function Interface) dla aplikacji .NET
// do komunikacji z TentaFlow.Router przez protokół QUIC z serializacją rkyv.
//
// JAK DZIAŁA:
// Eksportuje funkcje C-compatible które .NET wywołuje przez P/Invoke.
// Funkcje FFI konwertują typy między C i Rust, wywołują async klienta QUIC
// i zwracają wyniki w strukturach C-compatible.
//
// ARCHITEKTURA:
// .NET App → P/Invoke → tentaflow_client_native.so → QUIC+rkyv → Router
//
// PRZYKŁAD UŻYCIA:
// ```csharp
// [DllImport("tentaflow_client_native")]
// static extern IntPtr tentaflow_client_new(ref ClientConfigNative config);
//
// var client = tentaflow_client_new(ref config);
// var result = tentaflow_embeddings(client, "model", texts, count);
// ```
//
// KLUCZOWE KONCEPCJE:
// - FFI: Foreign Function Interface dla interoperacji C/.NET
// - P/Invoke: Platform Invocation Services w .NET
// - rkyv: Zero-copy serialization dla QUIC
// - tokio: Async runtime dla operacji sieciowych
//
// ============================================================================

mod ffi;
pub mod client;
pub mod chat_template;
mod types;

pub use ffi::*;
pub use chat_template::ChatTemplate;
pub use client::ChatCompletionOptions;
