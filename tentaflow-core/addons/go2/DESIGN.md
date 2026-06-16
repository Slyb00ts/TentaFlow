# Chunk 4b+ design — `go2` WASM addon

Ties the whole stack together: the addon owns ALL Go2 logic (signaling crypto,
commands, telemetry, safety), driving the generic `webrtc.*` host channel + the
backed-camera registration. Core stays a dumb pipe.

## SDK choice (ground-truth: match TentaVision)
TentaVision (flagship pro addon) uses **`tentaflow-sdk-spec` + minicbor + raw
externs**, NOT `tentaflow-addon-sdk`. The go2 addon follows the same pattern:
- deps: `tentaflow-sdk-spec` (CBOR types incl. the WebRtc* I added), `minicbor`,
  `serde_json`, `base64`, and **`tentaflow-hardware {default-features=false,
  features=["protocol"]}`** for the Go2 crypto/framing (parse_con_notify /
  build_con_ing / parse_con_ing_answer / validation_response / gen_session_key /
  heartbeat consts) — proven to compile to wasm32-wasip1.
- raw `#[link(wasm_import_module="tentaflow")] extern "C"` for host fns:
  `webrtc_connect_v1/set_answer_v1/state_v1/send_v1/drain_v1/close_v1/register_camera_v1`,
  `http_request` (signaling), `sql_*`, `store_*`, event publish/subscribe, UI send.
  Thin wrappers over them (mirror TentaVision's camera wrappers).

## Entry exports (TentaVision set)
`alloc`, `dealloc`, `on_install`, `on_start`, `on_stop`, `on_event(ptr,len)`,
`on_panel_open(id_ptr,id_len,epoch)`, `on_request(in,inlen,out,outcap,outlen)`,
**`on_tick(ts_ms)->i32`** (manifest `[service] enabled=true tick_interval_ms=1000`).

## Across-tick state machine (SQL-persisted — no busy-loop in one invocation)
State in the addon's SQLite (`robot` row): `status` ∈ {offline, connecting,
validating, online, error}, `channel_id`, `camera_id`, `battery_pct`, `rtt_ms`,
`last_update`, plus config `ip`.

- **`on_request "go2.connect"`** (UI action / tool): read ip; `webrtc_connect`
  with want_video + keepalive(HEARTBEAT_TEXT/MARKER) → (channel_id, offer_sdp);
  signaling: `http_request` con_notify → `protocol::parse_con_notify`;
  `protocol::build_con_ing` → `http_request` con_ing → `protocol::parse_con_ing_answer`
  → `webrtc_set_answer`. Persist channel_id + status="validating". Return. (NO
  validation loop here.)
- **`on_tick`** drives everything after connect:
  - status=="validating": `webrtc_drain` → if a `{"type":"validation","data":"<k>"}`
    msg and data!="Validation Ok." → `webrtc_send`(validation_response); if
    "Validation Ok." → `webrtc_register_camera` (bind video), subscribe lowstate +
    sportmodestate (webrtc_send subscribe), status="online".
  - status=="online": `webrtc_drain` → parse lowstate → battery (BMS SOC);
    `webrtc_state` → rtt_ms; persist; publish `go2.telemetry`; threshold checks →
    publish `go2.battery_low` / `go2.latency_high` (flows trigger on these events);
    detect peer Failed/Closed → status="error".
  - status=="error"/"offline": idle (or auto-reconnect later).

## Flow blocks (4c)
`blocks.json`: `go2.move` (vx,vy,vyaw,duration), `go2.pose` (roll,pitch,yaw,height),
`go2.action` (enum: hello/sit/stand_up/stand_down/recovery_stand/stretch/dance1/2),
`go2.stop` (e-stop → StopMove+Damp). `on_request "block.go2.*"` → build sport
request `{type:"req",topic:"rt/api/sport/request",data:{header:{identity:{id,api_id}},parameter}}`
→ `webrtc_send` on the stored channel_id. Safety: e-stop gate, velocity clamp,
require status==online. Air-locked motions flagged in block metadata.
Triggers = events (`go2.battery_low`/`go2.latency_high`/`go2.alarm`) consumed by
the existing event-trigger flow mechanism — no custom trigger block needed.

## Panel (4e) — on_panel_open
Status card: online/offline badge, **battery %**, **RTT ms**, peer state. Control
card: Connect/Disconnect, manual move/pose/action buttons, e-stop. Camera card:
live preview of the registered camera (reuse the streaming preview). Config:
robot IP. Built with `tentaflow_sdk_spec::protocol::ui::` types (TentaVision style).

## Manifest (already scaffolded at addons/go2/manifest.toml — extend)
Add `[service] enabled=true tick_interval_ms=1000`; `[[network_rule]]` host
192.168.0.190 port 9991 (con_notify/con_ing); permissions already include
webrtc.connect, http.request, cameras.write, events, sql.*, ui. `[storage] sql`.

## Codex fixes folded in (design final)
1. **Tick = 200ms** (not 1000) so the validation challenge is answered fast;
   throttle online telemetry/publish to every 5 ticks (~1s).
2. **CAS state transitions in SQL**: `offline/error -> connecting -> validating ->
   online`; reject `go2.connect` while connecting|validating|online. channel_id +
   camera_id live in SQL (host-side), never guest statics.
3. **Bounded connect**: set status=connecting BEFORE signaling; one in-flight
   connect; short HTTP timeout; on con_ing/set_answer failure → webrtc_close +
   status=error.
4. **getrandom OK** (wasmtime-wasi p1 random_get wired) — add a WASM smoke test
   calling gen_session_key()+build_con_ing().
5. Camera ownership: Go2-owned, TentaVision cross-addon deferred. (confirmed)
6. **E-stop durable + global**: `estop_active` in SQL, checked in EVERY
   move/pose/action path; only `go2.stop`/explicit reset clears; clamp all
   velocity/duration; on failed StopMove/Damp or high-latency/disconnect →
   `webrtc_close` (hard transport kill). **Air-locked motions are REJECTED at
   execution**, not just flagged.

## Open questions for codex (answered)
1. Is the SQL-persisted across-tick state machine the right shape, or is there a
   cleaner addon idiom for a long-lived connection? Any tick re-entrancy concern
   (tick N+1 firing while tick N still running — does the host serialize ticks)?
2. Signaling in on_request via http_request: con_notify+con_ing are 2 sequential
   blocking host calls inside ONE on_request invocation — acceptable, or should
   signaling also move to the tick? (Fuel/timeout budget for on_request.)
3. validation handled in tick (1s cadence) — the robot's validation challenge may
   arrive within ~1s of set_answer; is 1s tick fast enough, or risk timeout on the
   robot side? (Native spike validated in <1s via 50ms poll.)
4. getrandom at runtime in the addon sandbox (rsa needs it) — does wasmtime-wasi
   expose random_get to addons? (Compile passed; runtime unverified.)
5. Camera ownership: backed camera owned by go2 addon — TentaVision can't see it
   cross-addon. Confirm deferring TentaVision integration is right for v1.
6. e-stop: addon-side only (core is dumb pipe). Sufficient, or need a core-side
   kill on the channel?
