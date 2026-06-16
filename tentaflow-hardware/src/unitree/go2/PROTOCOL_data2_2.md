# Unitree Go2 WebRTC LAN protocol — `data2=2` (legacy, Go2 firmware < 1.1.15)

Reverse-engineered from `legion1581/unitree_webrtc_connect` (read-only reference).
Target robot: firmware 1.1.12, LAN signaling port `:9991` (con_notify). Port 8081
(legacy plaintext `/offer`, pre-1.1.11) is NOT used.

## 1. LAN signaling (HTTP, port 9991)

1. `POST http://<ip>:9991/con_notify` (no body).
   Response body = base64( JSON `{"data1": "<b64>", "data2": 2}` ).
2. Decrypt `data1` with the STATIC AES-128-GCM key (no per-device key on <1.1.15):
   - key = `[232,86,130,189,22,84,155,0,142,4,166,104,43,179,235,227]` (16 bytes)
   - raw = base64decode(data1); `ct = raw[..len-28]`, `nonce = raw[len-28..len-16]` (12B),
     `tag = raw[len-16..]`. AES-128-GCM decrypt(nonce, ct||tag) → UTF-8 string.
   - `data2 == 1` (or absent) → data1 is already plaintext. `data2 == 3` → per-device key (NOT us).
3. From decrypted `data1` (ASCII):
   - robot RSA public key (base64 DER, SPKI) = `data1[10 .. len-10]`
   - `path_ending` = take last 10 chars, split into 5 pairs, map each pair's SECOND char
     (`A..J` → `0..9`), concatenate the indices as a decimal string.
4. Build the WebRTC offer SDP (data channel "data" created BEFORE offer; video transceiver recvonly).
5. fresh session key = 32 random hex chars (used verbatim as 32 ASCII bytes = AES-256 key).
6. **The offer is wrapped in a JSON envelope BEFORE encryption** (raw SDP alone is silently
   dropped → 0-byte close): `offer_env = {"id":"STA_localNetwork","sdp":<offer_sdp>,"type":"offer","token":""}`.
   body = `{"data1": b64(AES-256-ECB/PKCS7(offer_env)), "data2": b64(RSA-PKCS1v1.5(session_key, robot_pubkey))}`
7. `POST http://<ip>:9991/con_ing_<path_ending>` (Content-Type application/x-www-form-urlencoded),
   body = JSON(body). HTTP must be **raw HTTP/1.0 + Connection: close** (embedded server does not
   handle hyper/reqwest HTTP-1.1 bodied POSTs; header terminator may be LFLF, not CRLFCRLF).
   Response text = AES-256-ECB(answer). Decrypt → JSON `{"sdp","type"}`. `sdp == "reject"` → robot
   busy (another WebRTC client). Else `sdp`/`type` → set remote description.

VERIFIED on real robot (fw 1.1.12, 192.168.0.188): data channel validated, sport command
acknowledged (rt/api/sport/response), rt/lf/lowstate telemetry streaming, inbound video
1280x720 @25fps H.264 (7 IDR + 197 P over 15s, clean ffprobe decode). webrtc-rs 0.17 handles
the inbound H.264 track fine (bug #164 did not trigger).

## 2. WebRTC peer
- DataChannel labeled `"data"` created before the offer.
- `addTransceiver("video", recvonly)` for the camera (H.264 inbound track).

## 3. Data channel validation (after channel "open")
- Robot sends `{"type":"validation","data":"<challenge>"}`.
- Respond `{"type":"validation","topic":"","data": b64( MD5_raw("UnitreeGo2_"+challenge) )}`.
  (MD5 → 16 raw bytes → base64; the JS does hex→bytes→base64 which equals base64 of raw digest.)
- Robot replies `{"type":"validation","data":"Validation Ok."}` → channel validated.

## 4. Message framing
- Outgoing: JSON string `{"type":..,"topic":..,"data":..}`.
- Incoming string → JSON. Incoming binary buffer:
  - first `<HH` (two u16 LE). If `(2,0)` → lidar: skip 4, `<I` len at [0], json [8..8+len], bin after.
  - else normal: `<H` len at [0], json [4..4+len], bin at [4+len..].

## 5. Topics & commands
- Sport request topic: `rt/api/sport/request`. Send a REQUEST (`type:"req"`):
  `{"type":"req","topic":"rt/api/sport/request","data":{"header":{"identity":{"id":<gen>,"api_id":<id>}},"parameter":<str or "">}}`
  - StandUp=1004, StandDown=1005, RecoveryStand=1006, Hello=1016, Move=1008
    (Move parameter = JSON string `{"x":vx,"y":vy,"z":vyaw}`), StopMove=1003, Damp=1001.
- Telemetry subscribe (`type:"subscribe"`, no data): `rt/lf/lowstate`, `rt/sportmodestate`.
- Video on: `{"type":"vid","topic":"","data":"on"}` (publish_without_callback). Frames then on the video track.
- LiDAR: subscribe `rt/utlidar/voxel_map_compressed` AFTER `{"type":"msg","topic":"rt/utlidar/switch","data":"on"}`.
- Heartbeat: periodic keepalive on the channel (see msgs/heartbeat.py) — required for long sessions.

## 6. Crypto crates (Rust)
- AES-128-GCM: `aes-gcm` (already a repo dep).
- AES-256-ECB/PKCS7: `aes` + `ecb`.
- RSA PKCS1v1.5 encrypt: `rsa` (SPKI DER via `from_public_key_der`).
- MD5: `md-5`. base64: `base64`. hex: `hex`.

## 7. Constraints
- ONE WebRTC client at a time — the phone app must be disconnected (else `con_ing` / ICE fails).
- Robot at 192.168.0.188 (later .190). `:9991` confirmed OPEN, `:8081` closed (→ con_notify path).
