# Chunk 2 design — WebRTC video track → TentaVision-style camera (first appsrc)

Goal: turn the inbound WebRTC video track of a generic channel into a normal
camera the existing camera_ingest pipeline (detector / streaming / snapshots)
consumes — so a robot camera reuses the whole TentaVision stack. This is the
first GStreamer `appsrc` in the repo.

## Boundary (keep webrtc-rs out of core's camera_ingest)
The channel (tentaflow-hardware) depacketizes RTP → H.264 **Annex-B byte stream**
and exposes it as a `tokio::mpsc::Receiver<Vec<u8>>`. camera_ingest only sees
bytes — no `TrackRemote`/webrtc-rs types leak into core's camera path.

- `WebRtcChannel` (want_video): on_track spawns ONE reader that
  `track.read_rtp()` → H.264 depay (reuse the spike's H264Writer-equivalent /
  webrtc-rs `H264Packet`) → push Annex-B chunk to an internal `broadcast`/`mpsc`.
  Until a sink is attached the reader **discards** (bounded, no buffering leak).
- `channel.take_h264_rx() -> Option<Receiver<Vec<u8>>>` hands the byte stream to
  exactly one consumer (the camera pump). Single-consumer.

## camera_ingest changes (core)
- `SessionSource::WebRtc(tokio::mpsc::Receiver<Vec<u8>>)` new variant.
- `build_webrtc_pipeline`: `appsrc(caps=video/x-h264,stream-format=byte-stream) →
  h264parse → (nvh264dec|avdec_h264) → videoconvert → RGB caps → appsink` and
  reuse the existing `install_frame_callback` at the RGB appsink (frame_storage +
  streaming_bus, unchanged). A pump task drains the Receiver → `appsrc.push_buffer`.
- vendor dispatch (`session.rs:188`): add `"webrtc" => spawn_webrtc_inner(config, rx)`.
  The rx is threaded in out-of-band (not from `url`) since the source is a live
  handle, not a URL.

## Host ABI
- Re-allow `want_video=true` in `webrtc_connect_v1` (Chunk 1b rejected it). Safe:
  Go2 video stays off until the addon sends `vid:on` on the data channel, and the
  channel reader discards until bound — no leaking receiver.
- New `camera_register_backed_v1(channel_id, display_name, target_fps, analysis_fps)
  -> camera_id`:
  - perms: `cameras.write` (+ the addon already holds `webrtc.connect`).
  - look up the addon's channel; `take_h264_rx()` (error if no video / already taken).
  - supervisor `add_camera` with `SessionSource::WebRtc(rx)`, vendor `"webrtc"`,
    `url = channel_id` (marker), `owner_addon_id = addon`.
  - insert camera row (vendor `webrtc`, status active) so it lists like any camera.
- Unbind path: `camera_remove_v1` already exists; extend it to stop the backed
  session. Also `cleanup_addon_channels` (Chunk 1b) must drop backed cameras for
  the closed channel (supervisor remove + row delete/offline).

## Lifecycle / ownership
- **Not hydrated on boot**: `hydrate_supervisor_from_db` skips vendor `"webrtc"`
  (the live track is gone after restart). Row kept as `offline` until the addon
  re-binds, or deleted on close — decide below.
- Camera owned by the **registering addon** (go2). Cross-addon visibility into
  TentaVision (so TentaVision flows can run on the robot camera) is a SEPARATE
  later decision (user: TentaVision↔Robots split is TBD). Chunk 2 makes the
  backed camera work + previewable by its owner; TentaVision wiring is follow-up.

## Open questions for codex
1. Persist the backed camera row, or keep it runtime-only? If persisted, exact
   hydrate-skip + reconcile-on-rebind semantics; if runtime-only, how does it
   appear in camera_list (which reads DB)?
2. appsrc lifecycle: caps/timestamps (do we need PTS, or is `do-timestamp=true`
   enough?), need-data/enough-data backpressure, EOS on channel close, draining
   the pump task on session Stop, GStreamer state transitions. This is the
   riskiest piece (first appsrc).
3. Depay correctness: reuse webrtc-rs H264 depacketizer vs the H264Writer path;
   keyframe-wait on (re)bind so the decoder starts on an IDR (avoid the
   `non-existing PPS 0` decode errors seen with mid-stream starts).
4. Should `register_backed` enable video itself (send vid:on) or leave that to
   the addon? (Addon owns Go2 protocol — likely the addon enables it AFTER bind.)
5. Cross-addon camera sharing for TentaVision — defer entirely, or design the
   hook now (e.g. an org-visible / shared flag on the camera)?
