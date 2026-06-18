// =============================================================================
// File: services/stream_hub/source.rs — BinaryStreamSource trait + factory alias
// =============================================================================
//
// A source is any producer that pushes opaque binary chunks (fMP4 fragments,
// MJPEG frames, raw audio frames, ...) into a `tokio::sync::broadcast` channel.
// The hub fans the chunks out to N subscribers (browsers, addons) without the
// producer caring how many are attached. MIME type is the MSE-grade descriptor
// the browser needs to construct a SourceBuffer. The optional init segment is
// the codec-specific preamble (ftyp+moov for fMP4) that every new subscriber
// must receive before media chunks.

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::broadcast;

use super::error::StreamHubError;

/// Broadcast channel capacity per source.
///
/// Slow subscribers that lag behind by more than this many chunks receive
/// `RecvError::Lagged(n)`; the upstream WS handler (Chunk B) interprets that
/// as "drop the peer, let it reconnect from the init segment".
pub const BROADCAST_CAPACITY: usize = 32;

/// Producer-facing description of a binary stream.
///
/// Implementations are addon- or service-specific (e.g. one per camera, one
/// per microphone). The hub owns the `Arc<dyn BinaryStreamSource>` once the
/// factory yields it and drops it when the last subscriber goes away.
#[async_trait::async_trait]
pub trait BinaryStreamSource: Send + Sync {
    /// Stable identifier such as `camera:cam_xxx` or `audio:doorbell`.
    fn id(&self) -> &str;

    /// MSE-compatible MIME type, e.g. `video/mp4; codecs="avc1.4D4028"`.
    fn mime_type(&self) -> &str;

    /// Init segment delivered once to every new subscriber. `None` means each
    /// chunk on the broadcast channel is self-contained (e.g. MJPEG frames).
    async fn init_segment(&self) -> Option<Bytes>;

    /// Broadcast sender the source pushes media chunks into. The hub hands
    /// out fresh receivers via `Sender::subscribe()` on every subscribe call.
    ///
    /// Returns `None` when the source has terminally failed (e.g. a remote
    /// relay that never received an init segment): the hub then treats the
    /// subscribe as a clean failure instead of registering a hung empty stream.
    /// Live sources always return `Some`.
    fn chunk_broadcaster(&self) -> Option<broadcast::Sender<Bytes>>;
}

/// Factory invoked the first time a stream is subscribed to. The hub caches
/// the resulting `Arc` until the subscriber count drops to zero.
pub type StreamSourceFactory =
    Box<dyn Fn() -> Result<Arc<dyn BinaryStreamSource>, StreamHubError> + Send + Sync + 'static>;
