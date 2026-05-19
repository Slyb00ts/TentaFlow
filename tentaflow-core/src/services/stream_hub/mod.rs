// =============================================================================
// File: services/stream_hub/mod.rs — public API for the binary stream hub
// =============================================================================
//
// Chunk A of the WS streaming foundation: a generic pub/sub layer that lets
// any producer (camera ingest, audio capture, addon services) advertise a
// named binary stream and lets any consumer (WS handler in Chunk B) subscribe
// to it with MSE-grade metadata (MIME + init segment) plus a backpressure-
// aware `tokio::sync::broadcast` receiver. The hub owns subscriber refcounts
// and tears the source down when the last consumer goes away.

mod error;
mod manager;
mod source;
mod subscription;

#[cfg(test)]
mod tests;

pub use error::StreamHubError;
pub use manager::StreamHub;
pub use source::{BinaryStreamSource, StreamSourceFactory, BROADCAST_CAPACITY};
pub use subscription::SubscriptionHandle;
