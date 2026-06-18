// =============================================================================
// File: services/camera_relay/mod.rs — cross-node live camera relay
// =============================================================================
//
// Lets an observer node (B) watch a robot camera physically owned by another
// node (A) as a SMOOTH MSE stream, reusing the local `StreamHub` plumbing so
// `tf-video-stream stream-id="camera:<id>"` is identical for local and remote
// cameras.
//
// Wire: a dedicated QUIC bi-stream (discriminator
// `MESH_MSG_CAMERA_STREAM_SUBSCRIBE`), NOT a UFP/2 unicast envelope. B opens
// the bi-stream, sends a `CameraStreamSubscribePayload`, A subscribes to its
// own local `camera:<id>` StreamHub source and streams fMP4 frames
// (`CameraStreamFrame`) back. QUIC flow-control provides natural backpressure.
//
//   - `server`: owner-side (A) handler — subscribes to the local StreamHub and
//     pumps the init segment + media chunks down the bi-stream.
//   - `source`: observer-side (B) `BinaryStreamSource` — republishes the
//     relayed chunks into the local StreamHub so the dashboard tile is unaware
//     the camera lives on another node.

pub mod server;
pub mod source;
