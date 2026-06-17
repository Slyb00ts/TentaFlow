# Chunk 1b design — generic `webrtc.*` host ABI for WASM addons

Goal: expose the generic `WebRtcChannel` (tentaflow-hardware) to WASM addons as a
dumb pipe. The addon drives signaling (offer out, it does con_notify/con_ing +
crypto itself via `http.request`, answer in) and the data channel (send/drain).
Core knows nothing robot-specific. Mirrors the proven `streaming.rs` pattern.

## Registry & scoping
- `static CHANNELS: OnceLock<DashMap<(addon_id, channel_id), Arc<WebRtcChannel>>>`.
- `channel_id` = opaque `"webrtc_<uuid>"`, returned by connect. Every call is
  scoped to `caller.data().addon_id` — an addon can only touch its own channels.
- Quota: `MAX_CHANNELS_PER_ADDON = 8`, `MAX_CHANNELS_GLOBAL = 64` (reject with
  Conflict/RateLimit past the cap).
- Permission gate: single `webrtc.connect` on every call (open + use). Robot
  movement safety lives in the addon, not here (dumb pipe).
- Feature-gated behind cargo feature `webrtc` in tentaflow-core (pulls
  tentaflow-hardware + webrtc-rs; off by default so headless builds stay lean).

## Async bridge
Host fns are sync; `WebRtcChannel` is async. Use the `run_async` block_in_place
bridge (streaming.rs:70). `connect` blocks up to `gather_timeout_ms` (one-time).

## Host functions (all 5-param CBOR ABI: in_ptr,in_len,out_ptr,out_cap,out_len_ptr)

1. `webrtc_connect_v1(WebRtcConnectInput) -> WebRtcConnectOutput`
   - in: `data_channel_label`, `want_video`, `disable_mdns`, `gather_timeout_ms`, `inbound_capacity`
   - creates WebRtcChannel, gathers ICE, stores in registry.
   - out: `channel_id`, `offer_sdp`.
2. `webrtc_set_answer_v1(WebRtcSetAnswerInput) -> WebRtcStatusOutput`
   - in: `channel_id`, `answer_sdp`. out: `ok`.
3. `webrtc_state_v1(WebRtcStateInput) -> WebRtcStateOutput`
   - in: `channel_id`. out: `peer_state` (string), `dc_open`, `dropped_count`, `queue_len`.
4. `webrtc_send_v1(WebRtcSendInput) -> WebRtcStatusOutput`
   - in: `channel_id`, `is_text`, `data` (bytes; text is utf8). out: `ok`.
   - preflight: dc_open; else NotFound/Operation (addon retries).
5. `webrtc_drain_v1(WebRtcDrainInput) -> WebRtcDrainOutput`
   - in: `channel_id`, `max_messages`. out: `messages: [{is_text, data}]`,
     `dropped_count`, `queue_len` (remaining), `closed`.
   - needs new `WebRtcChannel::dc_drain_n(max)` (bounded drain).
6. `webrtc_close_v1(WebRtcCloseInput) -> WebRtcStatusOutput`
   - in: `channel_id`. removes from registry → Drop closes pc. out: `closed`.

## CBOR protocol structs — tentaflow-sdk-spec/src/protocol/webrtc.rs
`#[cbor(map)] #[n(N)]`, hand-written (no codegen). One `WebRtcMessage { is_text, data }`.
`WebRtcStatusOutput { ok }` shared by set_answer/send/close.

## SDK wrappers — addon-sdk/sdk/src/lib.rs
`webrtc_connect(cfg) -> (channel_id, offer_sdp)`, `webrtc_set_answer`, `webrtc_state`,
`webrtc_send_text`/`webrtc_send_binary`, `webrtc_drain(channel_id, max)`, `webrtc_close`.
Re-export in prelude. Extern decls + `webrtc.connect` in manifest permissions.

## Cleanup (deterministic, codex-required)
- `pub fn cleanup_addon_channels(addon_id)` in the host webrtc module: remove all
  registry entries for that addon (Drop closes each pc).
- Call it from `AddonManager::unregister_addon_runtime` (mod.rs ~1090) — covers
  disable / uninstall / sync-unload / update. Also from `stop_addon` network-close.

## Inbound model (codex pull-model verdict)
- Buffering starts at `connect` (on_message registered before answer), so pre-poll
  messages are not missed.
- ONE consumer per channel (the owning addon) — no shared drain, so no cursor/seq
  needed for v1; `dropped_count` surfaces overflow loss. Per-message seq deferred.
- `drain` is destructive but single-owner; the addon's own protocol logic decides
  what to keep (e.g. Go2 validation). No filtered/peek in v1 (note for later if a
  second consumer ever needs it).

## Codex review — folded in (design now implementation-ready)
1. **Retry-safe drain (Go2-critical).** CBOR output uses retry-on-small-buffer
   (host returns required size, addon repeats). A destructive drain would LOSE
   messages on `OutputBufferTooSmall`. Fix: each registry entry holds a host-side
   `pending: Vec<DcMessage>` staging buffer. `drain` = if `pending` empty, pull up
   to `max_messages` from the channel via `dc_drain_n`; encode+write `pending`; on
   success clear it; on too-small leave `pending` for the retry. Never lose the
   Go2 validation challenge/response.
2. **Explicit async close, not Drop.** `close`/`cleanup_addon_channels` call
   `channel.close()` via `run_async` THEN remove from registry. `Drop` is only a
   best-effort fallback, not the lifecycle contract.
3. **Serialize per-channel ops.** Registry value = `Arc<ChannelEntry>` where
   `ChannelEntry { chan: WebRtcChannel, lock: tokio::Mutex<()>, pending: ... }`;
   `set_answer`/`send`/`drain`/`close` take the per-entry lock so concurrent host
   calls on one channel_id can't race.
4. **Clamp everything + correct error vocab.** `MAX_GATHER_TIMEOUT_MS=15000`,
   `MAX_INBOUND_CAPACITY=8192`, `MAX_MESSAGES=256`, `MAX_MESSAGE_BYTES=256*1024`,
   `MAX_DRAIN_BYTES=4*1024*1024`. Quota over cap → `AbiError::QuotaExceeded`
   (not Conflict/RateLimit).
5. **Send preflight error clarity.** Missing channel_id → `NotFound`. Channel
   exists but dc not open → `Operation` (addon polls `state.dc_open` and retries);
   never overload `NotFound`.
6. **Validate inputs.** `channel_id_valid` (like `stream_id_valid`); reject
   `is_text=true` payloads that are not valid UTF-8 before send.

## Explicitly deferred
- Video track → camera binding is Chunk 2 (`camera.register_backed`; at the addon
  boundary video is a camera handle, never a raw TrackRemote). **In 1b the host
  ABI REJECTS `want_video=true`** (`Unsupported`) so no unconsumed/leaking video
  receiver is created before binding exists. (The native spike still uses
  `want_video=true` directly on WebRtcChannel — it reads the track itself.)
- send-side `buffered_amount` backpressure (webrtc-rs SCTP buffers; add if needed).
- per-message monotonic seq / peek-ack (only if multi-consumer arrives).
