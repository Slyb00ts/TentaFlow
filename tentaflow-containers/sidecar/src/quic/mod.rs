// =============================================================================
// Plik: quic/mod.rs
// Opis: QUIC server sidecara — akceptuje polaczenia od routera TentaFlow,
//       obsluguje ModelRequest → Handler, zwraca ModelResponse lub streamuje
//       ModelStreamChunk. Niezaleznie od roli (LLM/STT/TTS/...) — rola wstrzykuje
//       tylko implementacje `Handler`.
//
//       Mechanizmy niezawodnosci:
//       - Quinn `keep_alive_interval` — natywny PING na poziomie QUIC co 10s
//       - Quinn `max_idle_timeout` — 30s, automatyczne wykrycie zerwanego peera
//       - Graceful shutdown: `Endpoint::close()` wysyla CONNECTION_CLOSE do wszystkich
//       - Kazde polaczenie ma `closed()` future ktore konczy sie z powodem rozlaczenia
// =============================================================================

pub mod handler;
pub mod protocol;
pub mod server;

pub use handler::{Handler, HandlerError};
pub use server::{QuicServer, QuicServerConfig};
