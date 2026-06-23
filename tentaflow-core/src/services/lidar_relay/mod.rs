// =============================================================================
// File: services/lidar_relay/mod.rs — cross-node live LiDAR relay
// =============================================================================
//
// Lets an observer node (B) view a robot's LiDAR point cloud physically owned by
// another node (A) over the SAME `StreamHub` rails the local push uses, so
// subscribing to `lidar:<robot_id>` is identical for local and remote robots.
//
// Wire: a dedicated QUIC bi-stream (discriminator
// `MESH_MSG_LIDAR_STREAM_SUBSCRIBE`), NOT a UFP/2 unicast envelope. B opens the
// bi-stream, sends a `LidarStreamSubscribePayload`, A subscribes to its own local
// `lidar:<robot_id>` StreamHub source (the `LocalLidarStreamSource` pump) and
// streams canonical frames (`LidarStreamFrame`) back. QUIC flow-control provides
// natural backpressure.
//
// Mirror of `camera_relay` MINUS the init-segment machinery: LiDAR frames are
// self-describing, so there is no separate codec preamble — every frame is a
// complete point cloud and the observer treats the latest received frame as its
// dynamic init segment.
//
//   - `server`: owner-side (A) handler — subscribes to the local StreamHub and
//     pumps frames down the bi-stream.
//   - `source`: observer-side (B) `BinaryStreamSource` — republishes the relayed
//     frames into the local StreamHub so the dashboard tile is unaware the robot
//     lives on another node.

pub mod server;
pub mod source;
