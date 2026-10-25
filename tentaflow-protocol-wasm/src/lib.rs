// =============================================================================
// Plik: tentaflow-protocol-wasm/src/lib.rs
// Opis: WASM bindings for the browser codec: Envelope helpers, the generic
//       schema-driven body encoder/decoder (`encodeBody`, `decodeMessageBody`)
//       and the UI-channel / LiDAR decoders. Request shapes are built in JS
//       (`js/protocol/requests.js`); no typed protocol `Deserialize` is linked.
// =============================================================================

use std::collections::HashMap;
use std::sync::OnceLock;
use tentaflow_protocol::{
    envelope::{message_kind, Envelope, EnvelopeFlags, Routing},
    SCHEMA_VERSION as PROTOCOL_SCHEMA_VERSION,
};
mod body;
use wasm_bindgen::prelude::*;

mod identity;
pub use identity::*;

// =============================================================================
// Init
// =============================================================================

/// Inicjalizacja modulu — ustawia panic hook dla lepszych bledow w console.
/// Wolane raz po zaladowaniu .wasm w przegladarce.
#[wasm_bindgen(start)]
pub fn wasm_main() {
    #[cfg(feature = "console_error_panic_hook")]
    console_error_panic_hook::set_once();
}

/// Wersja schematu protokolu. MUSI byc zgodna ze `tentaflow_protocol::SCHEMA_VERSION`
/// po stronie serwera — handshake sprawdza match, mismatch = reject connection.
#[wasm_bindgen(js_name = SCHEMA_VERSION)]
pub fn schema_version() -> u16 {
    PROTOCOL_SCHEMA_VERSION
}

// =============================================================================
// Message kind constants (exported as JS object)
// =============================================================================

/// Stale discriminantow message_kind dla dispatchu po stronie JS.
/// Wolac `messageKind()` raz, cachowac result.
#[wasm_bindgen(js_name = messageKind)]
pub fn message_kind_map() -> JsValue {
    let obj = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &obj,
        &"META_SCHEMA_VERSION_CHECK".into(),
        &(message_kind::META_SCHEMA_VERSION_CHECK as u32).into(),
    );
    let _ = js_sys::Reflect::set(
        &obj,
        &"META_PROTOCOL_ERROR".into(),
        &(message_kind::META_PROTOCOL_ERROR as u32).into(),
    );
    let _ = js_sys::Reflect::set(
        &obj,
        &"META_HEARTBEAT".into(),
        &(message_kind::META_HEARTBEAT as u32).into(),
    );
    let _ = js_sys::Reflect::set(
        &obj,
        &"META_CANCEL_STREAM".into(),
        &(message_kind::META_CANCEL_STREAM as u32).into(),
    );
    obj.into()
}

// =============================================================================
// Envelope encode / decode
// =============================================================================

/// Pure-Rust implementacja (testowalna bez wasm-bindgen shima).
fn encode_envelope_direct_inner(
    correlation_id: u64,
    sequence: u64,
    message_kind: u16,
    body: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let env = Envelope::new_direct(correlation_id, sequence, message_kind, body);
    tentaflow_protocol::cbor::encode(&env)
        .map(|v| v.to_vec())
        .map_err(|e| format!("envelope encode failed: {e}"))
}

/// Buduje Envelope (routing=Direct) z podanymi polami + body bytes; zwraca
/// CBOR-zakodowany frame jako Uint8Array.
///
/// `correlation_id` przekazywany jako u64 (BigInt po stronie JS).
#[wasm_bindgen(js_name = encodeEnvelopeDirect)]
pub fn encode_envelope_direct(
    correlation_id: u64,
    sequence: u64,
    message_kind: u16,
    body: Vec<u8>,
) -> Result<Vec<u8>, JsError> {
    encode_envelope_direct_inner(correlation_id, sequence, message_kind, body)
        .map_err(|e| JsError::new(&e))
}

/// Przepina gotowy frame na Routing::Forward do wskazanego wezla (64-znakowy
/// hex Ed25519 node id). Jedna funkcja zamiast wariantu Forward w kazdym
/// encoderze: klient buduje frame jak zwykle i opcjonalnie go adresuje.
/// Serwer (wezel dashboardu) mintuje asercje i przekazuje body przez mesh —
/// klient nie niesie zadnego materialu kryptograficznego.
#[wasm_bindgen(js_name = envelopeSetForward)]
pub fn envelope_set_forward(frame: &[u8], target_node_hex: &str) -> Result<Vec<u8>, JsError> {
    let decoded = hex::decode(target_node_hex)
        .map_err(|e| JsError::new(&format!("target node id is not hex: {e}")))?;
    let target: [u8; 32] = decoded
        .try_into()
        .map_err(|_| JsError::new("target node id must be 32 bytes of hex"))?;
    let mut env = tentaflow_protocol::cbor::decode::<Envelope>(frame)
        .map_err(|e| JsError::new(&format!("envelope decode failed: {e}")))?;
    env.routing = Routing::Forward {
        target_node_id: target,
    };
    tentaflow_protocol::cbor::encode(&env)
        .map(|v| v.to_vec())
        .map_err(|e| JsError::new(&format!("envelope encode failed: {e}")))
}

/// Widok zdekodowanego envelope'u wystawiony do JS. Body wyciete jako osobny
/// Uint8Array zeby call-site mogl zdekodowac MessageBody osobno.
#[wasm_bindgen]
pub struct EnvelopeView {
    #[wasm_bindgen(readonly)]
    pub schema_version: u16,
    #[wasm_bindgen(readonly)]
    pub correlation_id: u64,
    #[wasm_bindgen(readonly)]
    pub sequence: u64,
    #[wasm_bindgen(readonly)]
    pub message_kind: u16,
    #[wasm_bindgen(readonly)]
    pub flags: u8,
    #[wasm_bindgen(readonly)]
    pub is_forward: bool,
    target_node_id: Option<Vec<u8>>,
    body: Vec<u8>,
}

#[wasm_bindgen]
impl EnvelopeView {
    /// 32-byte target node id jesli Routing::Forward, inaczej None.
    #[wasm_bindgen(getter, js_name = targetNodeId)]
    pub fn target_node_id(&self) -> Option<Vec<u8>> {
        self.target_node_id.clone()
    }

    /// CBOR-zakodowany MessageBody — przekazac do `decodeMessageBody()`.
    #[wasm_bindgen(getter)]
    pub fn body(&self) -> Vec<u8> {
        self.body.clone()
    }

    /// True jesli flaga `IS_ERROR` ustawiona (body = `MessageBody::Error`).
    #[wasm_bindgen(getter, js_name = isError)]
    pub fn is_error(&self) -> bool {
        (self.flags & EnvelopeFlags::IS_ERROR.bits()) != 0
    }

    /// True jesli flaga `IS_STREAM_CHUNK` ustawiona.
    #[wasm_bindgen(getter, js_name = isStreamChunk)]
    pub fn is_stream_chunk(&self) -> bool {
        (self.flags & EnvelopeFlags::IS_STREAM_CHUNK.bits()) != 0
    }

    /// True jesli flaga `IS_STREAM_END` ustawiona.
    #[wasm_bindgen(getter, js_name = isStreamEnd)]
    pub fn is_stream_end(&self) -> bool {
        (self.flags & EnvelopeFlags::IS_STREAM_END.bits()) != 0
    }
}

/// Decode + bytecheck (NIGDY `access_unchecked`) pelnego envelope'u z WSS input.
/// Zwraca strukturalny widok; body wciaz zakodowany (lazy decode przez
/// `decodeMessageBody`).
#[wasm_bindgen(js_name = decodeEnvelope)]
pub fn decode_envelope(bytes: &[u8]) -> Result<EnvelopeView, JsError> {
    let env = tentaflow_protocol::cbor::decode::<Envelope>(bytes)
        .map_err(|e| JsError::new(&format!("envelope decode failed: {e}")))?;

    let (is_forward, target_node_id) = match env.routing {
        Routing::Direct => (false, None),
        Routing::Forward { target_node_id } => (true, Some(target_node_id.to_vec())),
    };

    Ok(EnvelopeView {
        schema_version: env.schema_version,
        correlation_id: env.correlation_id,
        sequence: env.sequence,
        message_kind: env.message_kind,
        flags: env.flags.bits(),
        is_forward,
        target_node_id,
        body: env.body,
    })
}

/// Szybka walidacja ze bajty maja prawidlowy ksztalt (pelny bytecheck envelope)
/// bez zwracania widoku. Uzyte do wczesnego odrzucenia malformed frames przed
/// enqueue do dispatch queue.
#[wasm_bindgen(js_name = validateFrame)]
pub fn validate_frame(bytes: &[u8]) -> bool {
    tentaflow_protocol::cbor::decode::<Envelope>(bytes).is_ok()
}

// =============================================================================
// MessageBody encode helpers (bootstrap typed constructors)
// =============================================================================

/// Wraps CBOR bytes of the UI channel as `MessageBody::UiChannelCbor` through the
/// wire schema, so the typed `MessageBody` serializer is not linked into the wasm.
fn encode_ui_channel_body(cbor_bytes: &[u8]) -> Result<Vec<u8>, JsError> {
    encode_ui_channel_body_native(cbor_bytes).map_err(|e| JsError::new(&e))
}

fn encode_ui_channel_body_native(cbor_bytes: &[u8]) -> Result<Vec<u8>, String> {
    tentaflow_codec_core::encode_body_with(
        &[],
        "UiChannelCbor",
        &serde_json::json!({ "$bytes": 0 }),
        &[cbor_bytes],
    )
    .map_err(|e| format!("body encode failed: {e}"))
}

fn parse_json(text: &str, what: &str) -> Result<serde_json::Value, JsError> {
    serde_json::from_str(text).map_err(|e| JsError::new(&format!("invalid {what} json: {e}")))
}

/// Encodes a request the browser holds as JSON through the wire schema, so no
/// typed `Deserialize` of the protocol is linked into the wasm. `path` lists
/// the enclosing variants outermost first, `variant` is the leaf.
fn json_body(path: &[&str], variant: &str, fields: &serde_json::Value) -> Result<Vec<u8>, JsError> {
    tentaflow_codec_core::encode_body(path, variant, fields)
        .map_err(|e| JsError::new(&format!("invalid {variant}: {e}")))
}

/// Encodes a request body from its JSON fields (snake_case wire names).
/// `path` is the dot-joined chain of enclosing variants, outermost first
/// (empty for a top-level `MessageBody` variant); `variant` is the leaf.
#[wasm_bindgen(js_name = encodeBody)]
pub fn encode_body(path: &str, variant: &str, fields_json: &str) -> Result<Vec<u8>, JsError> {
    let path: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    json_body(&path, variant, &parse_json(fields_json, variant)?)
}

/// `encodeBody` for a request with binary fields: the JSON refers to the n-th entry of
/// `blobs` as `{"$bytes": n}`, so a file crosses as a Uint8Array and never as a JSON array.
#[wasm_bindgen(js_name = encodeBodyWithBytes)]
pub fn encode_body_with_bytes(
    path: &str,
    variant: &str,
    fields_json: &str,
    blobs: js_sys::Array,
) -> Result<Vec<u8>, JsError> {
    let path: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    let blobs: Vec<Vec<u8>> = blobs
        .iter()
        .map(|blob| js_sys::Uint8Array::new(&blob).to_vec())
        .collect();
    let blob_refs: Vec<&[u8]> = blobs.iter().map(Vec::as_slice).collect();
    tentaflow_codec_core::encode_body_with(
        &path,
        variant,
        &parse_json(fields_json, variant)?,
        &blob_refs,
    )
    .map_err(|e| JsError::new(&format!("invalid {variant}: {e}")))
}

// ---- Mesh read-only views (FAZA 1a) ----

// ---- Sync baseline-adopt admin (FAZA C krok 3 — donor list/start/status/clear) ----

// ---- Mesh write ops (FAZA 1b — pairing/trust/connect/command/network-config) ----

// ---- Catalog + aliasy ----

// ---- SSO / TLS / NGC (FAZA 4) ----

// ---- Catalog: NIM + manifest deploy (FAZA 5) ----

// ---- Meeting VNC tunnel (same-node websockify bridge) ----

// ---- Meeting browser capture (screenshot / DOM snapshot) ----

// ---- Addons + Users (FAZA 6) ----

// ---- Multi-instance: katalog pakietow + operacje na instancjach ----

// =============================================================================
// Addon permissions + OAuth (migracja 38) — encodery request variantow
// =============================================================================

// ---- Audit log screen (Admin only) -------------------------------------

// ----- Benchmark Studio -----

// ----- TentaBus (SUM/tentabus/PLAN.md §6.2) -----

// ----- M2 replication (SUM/tentabus/PLAN-M2.md §1f) -----

// ----- Field policies (SUM/tentabus/POLITYKI-POL.md — F0) -----

// ----- Schema registry (SUM/tentabus/PLAN-F3.md §6) -----

// -----------------------------------------------------------------------------
// Storage admin (Ustawienia → Magazyn danych) — MessageBody::StorageAdminBody.
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// Events browser (Zarzadzanie -> Zdarzenia) — MessageBody::EventsBody.
//
// The browse filter is all-optional with a nested keyset cursor, so it crosses
// the boundary as a JSON object (the RoleCatalog precedent) instead of a dozen
// positional `Option<_>` parameters. `origins` keeps its three states apart:
// an absent/null key is NO constraint, an empty array matches NOTHING — the UI
// with every chip switched off must not be read as "show everything".
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// TF→ONNX model conversion (deploy wizard step, ROADMAP Z11) —
// MessageBody::ModelConversionBody.
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// Node environment identity + config-bundle pull (ROADMAP Z12) —
// MessageBody::EnvironmentPromotionBody.
// -----------------------------------------------------------------------------

// --- Models ---------------------------------------------------------------

// --- Hub ------------------------------------------------------------------

// --- Flows ----------------------------------------------------------------

// --- Services (Krok N2 — packed in `MessageBody::ServiceBody`) -----------

// --- Prompts --------------------------------------------------------------

// --- Meeting Bot ----------------------------------------------------------

// --- Registries -----------------------------------------------------------

// --- TTS rules ------------------------------------------------------------

// --- PII rules ------------------------------------------------------------

// --- Fast-path ------------------------------------------------------------

// --- Settings (multi-entry) -----------------------------------------------

// --- Model / alias access control (F1a §6.6) ------------------------------

// =============================================================================
// MessageBody decode (zwraca JS object z variant tag + polami)
// =============================================================================

fn set(obj: &js_sys::Object, key: &str, value: JsValue) {
    let _ = js_sys::Reflect::set(obj, &key.into(), &value);
}

/// Project a RAW canonical LiDAR frame (36-byte LE header + packed f32 body — the
/// bytes carried verbatim in a `StreamFrame.data` push) into the JS shape the
/// dashboard LiDAR consumer (L3a freshness) and the wgpu renderer (L4) expect.
/// The frame is parsed via the single source-of-truth header decoder
/// (`tentaflow_sdk_spec::LidarFrameHeader::decode_header`) so the fixed header
/// layout is never duplicated in JS. The returned object carries both the parsed
/// scalar fields and the raw/typed-array bodies: L3a reads `frameSeq`/`pointCount`
/// for freshness, L4 uploads `points` (world-space XYZ as a Float32Array, decoded
/// from whichever body layout the header declares) / `raw` (the full frame) to the
/// GPU. A malformed/short frame yields `{hasFrame: false}` rather than a fabricated
/// partial cloud.
fn lidar_frame_to_js(bytes: &[u8]) -> JsValue {
    use tentaflow_sdk_spec::{LidarFrameHeader, LIDAR_HEADER_LEN};
    let obj = js_sys::Object::new();
    let Some(header) = LidarFrameHeader::decode_header(bytes) else {
        set(&obj, "hasFrame", false.into());
        return obj.into();
    };
    // A frame is only "present" if the FULL declared body is actually here. An
    // unknown layout / overflowing `point_count` (`body_len() == None`) or a
    // short read (buffer ends before the declared frame) is treated as NO frame
    // rather than fabricating a clamped, mismatched cloud — `pointCount` must
    // always equal `points.length / layout`. Attacker-controlled bytes can only
    // reach the `{hasFrame:false}` path here; no `unwrap`/panic.
    let Some(body_len) = header.body_len() else {
        set(&obj, "hasFrame", false.into());
        return obj.into();
    };
    // Cap the inflate target derived from the (untrusted) header. The LZ4 path
    // allocates `body_len` zeroed bytes BEFORE validating the compressed input, so
    // a hostile header (huge point_count, tiny body) could otherwise OOM the tab.
    // 64 MiB covers any real frame (~0.5 MB) with vast headroom; larger = reject.
    const LIDAR_MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
    if body_len > LIDAR_MAX_BODY_BYTES {
        set(&obj, "hasFrame", false.into());
        return obj.into();
    }
    // Acquire the UNCOMPRESSED body: inflate the LZ4 block when flagged, else take
    // the declared slice. Either way `body.len() == body_len` afterward; a corrupt
    // block or short buffer yields `{hasFrame:false}` (never a partial cloud).
    let inflated;
    let body: &[u8] = if header.lz4_body() {
        match lz4_flex::block::decompress(&bytes[LIDAR_HEADER_LEN..], body_len) {
            Ok(d) if d.len() == body_len => {
                inflated = d;
                &inflated
            }
            _ => {
                set(&obj, "hasFrame", false.into());
                return obj.into();
            }
        }
    } else {
        let frame_end = LIDAR_HEADER_LEN + body_len;
        if bytes.len() < frame_end {
            set(&obj, "hasFrame", false.into());
            return obj.into();
        }
        &bytes[LIDAR_HEADER_LEN..frame_end]
    };
    set(&obj, "hasFrame", true.into());
    set(&obj, "frameSeq", header.frame_seq.into());
    set(&obj, "pointCount", header.point_count.into());
    set(&obj, "layout", header.layout.into());
    set(&obj, "resolution", header.resolution.into());
    let origin = js_sys::Array::new();
    for v in &header.origin {
        origin.push(&JsValue::from(*v as f64));
    }
    set(&obj, "origin", origin.into());
    set(&obj, "timestampUs", (header.timestamp_us as f64).into());
    set(&obj, "hostSendUs", (header.host_send_us as f64).into());
    // World-space XYZ as a Float32Array (what the renderer uploads). We build a
    // Rust `Vec<f32>` first and hand it over with a SINGLE bulk `Float32Array::from`
    // copy — per-element `set_index` across the wasm/JS boundary was the dominant
    // decode cost. For the packed-i16 grid layout we reconstruct world meters here
    // (`idx * resolution + origin`); for the f32 layouts the body is copied as-is.
    let floats: Vec<f32> = if header.layout == tentaflow_sdk_spec::LIDAR_LAYOUT_XYZ_I16_PLANAR {
        // Planar i16 grid: all ix, then all iy, then all iz. Reconstruct world
        // meters as `idx * resolution + origin` into interleaved XYZ for the GPU.
        let n = header.point_count as usize;
        let res = header.resolution;
        let [ox, oy, oz] = header.origin;
        let iy_base = n * 2;
        let iz_base = n * 4;
        let rd = |o: usize| i16::from_le_bytes([body[o], body[o + 1]]) as f32;
        let mut v = Vec::with_capacity(n * 3);
        for p in 0..n {
            v.push(rd(p * 2) * res + ox);
            v.push(rd(iy_base + p * 2) * res + oy);
            v.push(rd(iz_base + p * 2) * res + oz);
        }
        v
    } else {
        // f32 layouts (XYZ / XYZI): body is already little-endian f32 scalars.
        let count = body_len / 4;
        let mut v = Vec::with_capacity(count);
        for i in 0..count {
            let off = i * 4;
            v.push(f32::from_le_bytes([
                body[off],
                body[off + 1],
                body[off + 2],
                body[off + 3],
            ]));
        }
        v
    };
    set(
        &obj,
        "points",
        js_sys::Float32Array::from(&floats[..]).into(),
    );
    obj.into()
}

/// Decode the RAW canonical LiDAR frame bytes pushed in a `StreamFrame.data`
/// (L3a real-time PUSH stream `streamId = "lidar:<robot_id>"`) into the JS frame
/// projection. Reuses the sdk-spec header layout via `lidar_frame_to_js`; a
/// malformed/short frame returns `{hasFrame: false}` (no panic).
#[wasm_bindgen(js_name = decodeLidarFrame)]
pub fn decode_lidar_frame(bytes: &[u8]) -> JsValue {
    lidar_frame_to_js(bytes)
}

// =============================================================================
// Testy native — wolaja pure-Rust inner functions (bez wasm-bindgen JS shimow).
// Testy WASM-specyficzne (wasm-bindgen-test) doda sie pozniej gdy w CI bedziemy
// mieli wasm-pack test runner.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::message_body::MessageBody;

    #[test]
    fn protocol_schema_version_matches() {
        assert_eq!(PROTOCOL_SCHEMA_VERSION, tentaflow_protocol::SCHEMA_VERSION);
    }

    #[test]
    fn roundtrip_envelope_with_model_list_request() {
        let body = tentaflow_protocol::cbor::encode(&MessageBody::ModelListRequest).unwrap();
        let frame = encode_envelope_direct_inner(42, 1, message_kind::META_HEARTBEAT, body.clone())
            .unwrap();
        let env = tentaflow_protocol::cbor::decode::<Envelope>(&frame).unwrap();
        assert_eq!(env.correlation_id, 42);
        assert_eq!(env.sequence, 1);
        assert!(matches!(env.routing, Routing::Direct));
        assert_eq!(env.body, body);
    }

    #[test]
    fn validate_frame_accepts_good_and_rejects_bad() {
        let body = tentaflow_protocol::cbor::encode(&MessageBody::ModelListRequest).unwrap();
        let frame = encode_envelope_direct_inner(1, 1, 0xF001, body).unwrap();
        assert!(tentaflow_protocol::cbor::decode::<Envelope>(&frame).is_ok());
        assert!(tentaflow_protocol::cbor::decode::<Envelope>(&[]).is_err());
        assert!(tentaflow_protocol::cbor::decode::<Envelope>(&[0u8; 8]).is_err());
        assert!(tentaflow_protocol::cbor::decode::<Envelope>(&frame[..frame.len() / 2]).is_err());
    }

    #[test]
    fn ui_channel_body_matches_the_typed_encoding() {
        let raw = [1u8, 2, 3, 250];
        let schema = encode_ui_channel_body_native(&raw).unwrap();
        let typed =
            tentaflow_protocol::cbor::encode(&MessageBody::UiChannelCbor(raw.to_vec())).unwrap();
        assert_eq!(schema, typed);
    }

    #[test]
    fn body_encode_decode_round_trip_native() {
        let body = MessageBody::MetaHeartbeat {
            sent_at_epoch: 1_700_000_000,
        };
        let bytes = tentaflow_protocol::cbor::encode(&body).unwrap();
        let decoded = tentaflow_protocol::cbor::decode::<MessageBody>(&bytes).unwrap();
        assert_eq!(decoded, body);
    }
}

// =============================================================================
// UI Channel CBOR (Faza 6 Krok 4) — `MessageBody::UiChannelCbor(Vec<u8>)`.
// The CBOR bytes are opaque to the outer MessageBody; the browser JS codec encodes
// the UiPayload as CBOR itself and wraps it in this variant for transport.
// =============================================================================

/// Wraps raw CBOR bytes in `MessageBody::UiChannelCbor` for binary WS transport.
#[wasm_bindgen(js_name = encodeUiChannelCbor)]
pub fn encode_ui_channel_cbor(cbor_bytes: &[u8]) -> Result<Vec<u8>, JsError> {
    encode_ui_channel_body(cbor_bytes)
}

/// Encode PanelOpen into MessageBody::UiChannelCbor frame.
#[wasm_bindgen(js_name = encodeUiPanelOpen)]
pub fn encode_ui_panel_open(
    addon_id: String,
    panel_id: String,
    locale: String,
    theme: String,
    viewport_width: u32,
    viewport_height: u32,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_sdk_spec::protocol::ui::panel::{PanelOpen, PanelOpenContext, Viewport};
    use tentaflow_sdk_spec::UiPayload;

    let payload = UiPayload::PanelOpen(PanelOpen {
        addon_id,
        panel_id,
        ctx: PanelOpenContext {
            user_id: String::new(),
            locale,
            theme,
            viewport: Viewport {
                width_px: viewport_width,
                height_px: viewport_height,
                density: 1.0,
            },
            deep_link: None,
            prefers_reduced_motion: false,
            prefers_high_contrast: false,
            assigned_epoch: 0,
        },
    });

    encode_ui_payload_inner(&payload)
}

/// Encode PanelClose into MessageBody::UiChannelCbor frame.
#[wasm_bindgen(js_name = encodeUiPanelClose)]
pub fn encode_ui_panel_close(
    addon_id: String,
    panel_id: String,
    panel_epoch: u64,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_sdk_spec::protocol::ui::panel::{CloseReason, PanelClose};
    use tentaflow_sdk_spec::UiPayload;

    let payload = UiPayload::PanelClose(PanelClose {
        addon_id,
        panel_id,
        panel_epoch,
        reason: CloseReason::UserNavigated,
    });

    encode_ui_payload_inner(&payload)
}

/// Encode Action into MessageBody::UiChannelCbor frame.
#[wasm_bindgen(js_name = encodeUiAction)]
pub fn encode_ui_action(
    addon_id: String,
    panel_id: String,
    panel_epoch: u64,
    action_id: String,
    params_json: String,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_sdk_spec::protocol::ui::action::Action;
    use tentaflow_sdk_spec::UiPayload;

    let payload = UiPayload::Action(Action {
        addon_id,
        panel_id,
        panel_epoch,
        action_id,
        params: json_to_cbor_map(&params_json)?,
        form_values: None,
        user_gesture: true,
        client_action_id: tentaflow_sdk_spec::protocol::ids::ClientActionId::from_bytes(
            random_16_bytes(),
        ),
    });

    encode_ui_payload_inner(&payload)
}

/// Convert Vec<StateEntry> directly to JS array (no CBOR round-trip).
fn state_entries_to_js(
    entries: &[tentaflow_sdk_spec::protocol::ui::slot::StateEntry],
) -> Result<JsValue, JsError> {
    use tentaflow_sdk_spec::protocol::ui::bind::PathSegment;
    let arr = js_sys::Array::new();
    for entry in entries {
        let obj = js_sys::Object::new();
        let path_arr = js_sys::Array::new();
        for seg in &entry.path.segments {
            let seg_obj = js_sys::Object::new();
            match seg {
                PathSegment::Key(k) => {
                    set(&seg_obj, "kind", "key".into());
                    set(&seg_obj, "value", k.as_str().into());
                }
                PathSegment::Index(i) => {
                    set(&seg_obj, "kind", "index".into());
                    set(&seg_obj, "value", i.clone().into());
                }
            }
            path_arr.push(&seg_obj.into());
        }
        set(&obj, "path", path_arr.into());
        set(
            &obj,
            "value",
            value_to_js(&entry.value).map_err(|e| JsError::new(&e))?,
        );
        arr.push(&obj.into());
    }
    Ok(arr.into())
}

/// Convert Vec<PatchOp> directly to JS array (no CBOR round-trip).
fn patch_ops_to_js(
    ops: &[tentaflow_sdk_spec::protocol::ui::patch::PatchOp],
) -> Result<JsValue, JsError> {
    use tentaflow_sdk_spec::protocol::ui::bind::PathSegment;
    use tentaflow_sdk_spec::protocol::ui::patch::PatchOpKind;
    let arr = js_sys::Array::new();
    for op in ops {
        let obj = js_sys::Object::new();
        let path_arr = js_sys::Array::new();
        for seg in &op.path.segments {
            let seg_obj = js_sys::Object::new();
            match seg {
                PathSegment::Key(k) => {
                    set(&seg_obj, "kind", "key".into());
                    set(&seg_obj, "value", k.as_str().into());
                }
                PathSegment::Index(i) => {
                    set(&seg_obj, "kind", "index".into());
                    set(&seg_obj, "value", i.clone().into());
                }
            }
            path_arr.push(&seg_obj.into());
        }
        set(&obj, "path", path_arr.into());
        // StateStore.applyOpInPlace expects `op` as an object `{ kind, ... }`
        // (PatchOpKind tagged union) with the variant payload nested inside
        // `op`. Emitting `op` as a string with sibling fields is rejected
        // client-side, which silently drops every StatePatch.
        let op_obj = js_sys::Object::new();
        match &op.op {
            PatchOpKind::Set { value } => {
                set(&op_obj, "kind", "set".into());
                set(
                    &op_obj,
                    "value",
                    value_to_js(value).map_err(|e| JsError::new(&e))?,
                );
            }
            PatchOpKind::Delete => {
                set(&op_obj, "kind", "delete".into());
            }
            PatchOpKind::AppendArray { value } => {
                set(&op_obj, "kind", "append_array".into());
                set(
                    &op_obj,
                    "value",
                    value_to_js(value).map_err(|e| JsError::new(&e))?,
                );
            }
            PatchOpKind::PrependArray { value } => {
                set(&op_obj, "kind", "prepend_array".into());
                set(
                    &op_obj,
                    "value",
                    value_to_js(value).map_err(|e| JsError::new(&e))?,
                );
            }
            PatchOpKind::InsertArray { index, value } => {
                set(&op_obj, "kind", "insert_array".into());
                set(&op_obj, "index", index.clone().into());
                set(
                    &op_obj,
                    "value",
                    value_to_js(value).map_err(|e| JsError::new(&e))?,
                );
            }
            PatchOpKind::RemoveArray { index } => {
                set(&op_obj, "kind", "remove_array".into());
                set(&op_obj, "index", index.clone().into());
            }
            PatchOpKind::MergeMap { value } => {
                set(&op_obj, "kind", "merge_map".into());
                let m_obj = js_sys::Object::new();
                for (k, v) in &value.0 {
                    set(&m_obj, k, value_to_js(v).map_err(|e| JsError::new(&e))?);
                }
                set(&op_obj, "value", m_obj.into());
            }
            PatchOpKind::Increment { delta } => {
                set(&op_obj, "kind", "increment".into());
                set(&op_obj, "delta", delta.clone().into());
            }
        }
        set(&obj, "op", op_obj.into());
        arr.push(&obj.into());
    }
    Ok(arr.into())
}

/// Decode UI channel CBOR payload into a JS-friendly object.
#[wasm_bindgen(js_name = decodeUiPayload)]
pub fn decode_ui_payload(cbor_bytes: &[u8]) -> Result<JsValue, JsError> {
    use tentaflow_sdk_spec::UiPayload;

    let payload: UiPayload =
        minicbor::decode(cbor_bytes).map_err(|e| JsError::new(&format!("CBOR decode: {e}")))?;

    let obj = js_sys::Object::new();
    let tag = payload.tag().as_u16();
    set(&obj, "tag", tag.clone().into());

    match payload {
        UiPayload::PanelOpen(p) => {
            set(&obj, "addonId", p.addon_id.into());
            set(&obj, "panelId", p.panel_id.into());
            set(&obj, "assignedEpoch", p.ctx.assigned_epoch.clone().into());
        }
        UiPayload::PanelShell(s) => {
            set(&obj, "addonId", s.addon_id.into());
            set(&obj, "panelId", s.panel_id.into());
            set(&obj, "panelEpoch", s.panel_epoch.clone().into());
            // Layout Component decoded directly to JS — no re-encode round-trip
            set(
                &obj,
                "layout",
                component_to_js(&s.layout).map_err(|e| JsError::new(&e))?,
            );
            let slots = js_sys::Array::new();
            for slot in &s.slots {
                let s_obj = js_sys::Object::new();
                set(&s_obj, "id", slot.id.clone().into());
                // Emit the full slot policy so the JS SlotManager can apply
                // default-state/visibility/cache rules and so addon-app can
                // detect overlay slots (modal/drawer) and skip the static
                // container — id alone makes every slot look like main content.
                set(&s_obj, "semantics", slot.semantics.as_str().into());
                set(
                    &s_obj,
                    "default_state",
                    slot_default_to_js(&slot.default_state).map_err(|e| JsError::new(&e))?,
                );
                set(
                    &s_obj,
                    "cache_policy",
                    cache_policy_to_js(&slot.cache_policy)?,
                );
                set(
                    &s_obj,
                    "visibility",
                    slot_visibility_to_js(&slot.visibility)?,
                );
                if let Some(max) = slot.max_payload_bytes {
                    set(&s_obj, "max_payload_bytes", max.clone().into());
                }
                slots.push(&s_obj.into());
            }
            set(&obj, "slots", slots.into());
            // Initial state entries decoded directly to JS array
            set(&obj, "initialState", state_entries_to_js(&s.initial_state)?);
        }
        UiPayload::PanelReady(r) => {
            set(&obj, "addonId", r.addon_id.into());
            set(&obj, "panelId", r.panel_id.into());
            set(&obj, "panelEpoch", r.panel_epoch.clone().into());
        }
        UiPayload::PanelError(e) => {
            set(&obj, "addonId", e.addon_id.into());
            set(&obj, "panelId", e.panel_id.into());
            set(&obj, "message", e.message.into());
        }
        UiPayload::PanelClose(c) => {
            set(&obj, "addonId", c.addon_id.into());
            set(&obj, "panelId", c.panel_id.into());
            set(&obj, "panelEpoch", c.panel_epoch.clone().into());
        }
        UiPayload::PanelReset(r) => {
            set(&obj, "addonId", r.addon_id.into());
            set(&obj, "panelId", r.panel_id.into());
            set(&obj, "newPanelEpoch", r.new_panel_epoch.clone().into());
        }
        UiPayload::SlotContent(sc) => {
            set(&obj, "addonId", sc.addon_id.into());
            set(&obj, "panelId", sc.panel_id.into());
            set(&obj, "panelEpoch", sc.panel_epoch.clone().into());
            set(&obj, "slotId", sc.slot_id.into());
            set(
                &obj,
                "fragment",
                component_to_js(&sc.fragment).map_err(|e| JsError::new(&e))?,
            );
            // Atomic state seed shipped in the same wire frame as the fragment;
            // forwarded to SlotManager so bindings see seeded values before render.
            if let Some(overlay) = &sc.state_overlay {
                set(&obj, "stateOverlay", state_entries_to_js(overlay)?);
            }
        }
        UiPayload::SlotClear(c) => {
            set(&obj, "addonId", c.addon_id.into());
            set(&obj, "panelId", c.panel_id.into());
            set(&obj, "panelEpoch", c.panel_epoch.clone().into());
            set(&obj, "slotId", c.slot_id.into());
        }
        UiPayload::SlotShow(s) => {
            set(&obj, "addonId", s.addon_id.into());
            set(&obj, "panelId", s.panel_id.into());
            set(&obj, "panelEpoch", s.panel_epoch.clone().into());
            set(&obj, "slotId", s.slot_id.into());
        }
        UiPayload::SlotHide(h) => {
            set(&obj, "addonId", h.addon_id.into());
            set(&obj, "panelId", h.panel_id.into());
            set(&obj, "panelEpoch", h.panel_epoch.clone().into());
            set(&obj, "slotId", h.slot_id.into());
        }
        UiPayload::StateSnapshot(ss) => {
            set(&obj, "addonId", ss.addon_id.into());
            set(&obj, "panelId", ss.panel_id.into());
            set(&obj, "panelEpoch", ss.panel_epoch.clone().into());
            set(&obj, "stateRevision", ss.state_revision.clone().into());
            set(&obj, "entries", state_entries_to_js(&ss.entries)?);
            set(&obj, "truncated", ss.truncated.into());
        }
        UiPayload::StatePatch(sp) => {
            set(&obj, "addonId", sp.addon_id.into());
            set(&obj, "panelId", sp.panel_id.into());
            set(&obj, "panelEpoch", sp.panel_epoch.clone().into());
            set(&obj, "baseRevision", sp.base_revision.clone().into());
            set(&obj, "newRevision", sp.new_revision.clone().into());
            set(&obj, "ops", patch_ops_to_js(&sp.ops)?);
        }
        UiPayload::StateReset(sr) => {
            set(&obj, "addonId", sr.addon_id.into());
            set(&obj, "panelId", sr.panel_id.into());
            set(&obj, "panelEpoch", sr.panel_epoch.clone().into());
            set(&obj, "newRevision", sr.new_revision.clone().into());
        }
        UiPayload::PatchRejected(pr) => {
            set(&obj, "addonId", pr.addon_id.into());
            set(&obj, "panelId", pr.panel_id.into());
            set(&obj, "panelEpoch", pr.panel_epoch.clone().into());
            set(&obj, "rejectedMsgId", pr.rejected_msg_id.clone().into());
            if let Some(rev) = pr.current_revision {
                set(&obj, "currentRevision", rev.clone().into());
            }
        }
        UiPayload::Action(a) => {
            set(&obj, "addonId", a.addon_id.into());
            set(&obj, "panelId", a.panel_id.into());
            set(&obj, "panelEpoch", a.panel_epoch.clone().into());
            set(&obj, "actionId", a.action_id.into());
        }
        UiPayload::ActionAck(ack) => {
            set(&obj, "addonId", ack.addon_id.into());
            set(&obj, "panelId", ack.panel_id.into());
            set(&obj, "panelEpoch", ack.panel_epoch.clone().into());
            set(&obj, "actionId", ack.action_id.into());
            let status_str = match &ack.status {
                tentaflow_sdk_spec::protocol::ui::action::ActionStatus::Ok => "ok",
                tentaflow_sdk_spec::protocol::ui::action::ActionStatus::Error { .. } => "error",
                tentaflow_sdk_spec::protocol::ui::action::ActionStatus::Rejected { .. } => {
                    "rejected"
                }
                tentaflow_sdk_spec::protocol::ui::action::ActionStatus::PermissionDenied {
                    ..
                } => "permission_denied",
                tentaflow_sdk_spec::protocol::ui::action::ActionStatus::RateLimited { .. } => {
                    "rate_limited"
                }
                tentaflow_sdk_spec::protocol::ui::action::ActionStatus::ValidationFailed {
                    ..
                } => "validation_failed",
                tentaflow_sdk_spec::protocol::ui::action::ActionStatus::Redirected { .. } => {
                    "redirected"
                }
            };
            set(&obj, "status", status_str.into());
        }
        UiPayload::Command(_) => {
            // Full command decode not exposed; frontend uses raw CBOR via tag dispatch.
        }
        UiPayload::Event(ev) => {
            set(&obj, "sourceAddonId", ev.source_addon_id.into());
            // Encode topic as CBOR bytes for frontend topic matching
            let mut topic_cbor = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut topic_cbor);
            let _ = minicbor::Encode::encode(&ev.topic, &mut enc, &mut ());
            set(
                &obj,
                "topicCbor",
                js_sys::Uint8Array::from(&topic_cbor[..]).into(),
            );
            set(&obj, "tsMs", ev.ts_ms.clone().into());
        }
        UiPayload::Batch(_) => {
            // Batch is decoded member-by-member on frontend.
        }
    }

    Ok(obj.into())
}

// -- Private helpers for UI channel encode/decode --

/// Encodes a UiPayload to CBOR then wraps in MessageBody::UiChannelCbor.
fn encode_ui_payload_inner(payload: &tentaflow_sdk_spec::UiPayload) -> Result<Vec<u8>, JsError> {
    let mut cbor_buf = Vec::with_capacity(128);
    let mut enc = minicbor::Encoder::new(&mut cbor_buf);
    minicbor::Encode::encode(payload, &mut enc, &mut ())
        .map_err(|e| JsError::new(&format!("CBOR encode error: {e}")))?;
    encode_ui_channel_body(&cbor_buf)
}

/// Converts a JSON string to CborMap (Vec<(String, Value)>).
fn json_to_cbor_map(
    json_str: &str,
) -> Result<tentaflow_sdk_spec::protocol::control::CborMap, JsError> {
    use tentaflow_sdk_spec::protocol::control::CborMap;
    use tentaflow_sdk_spec::protocol::value::Value;

    let parsed: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| JsError::new(&format!("JSON parse: {e}")))?;

    let obj = match parsed {
        serde_json::Value::Object(m) => m,
        _ => return Ok(CborMap(Vec::new())),
    };

    let entries: Vec<(String, Value)> = obj
        .into_iter()
        .map(|(k, v)| (k, json_value_to_cbor_value(v)))
        .collect();

    Ok(CborMap(entries))
}

fn json_value_to_cbor_value(v: serde_json::Value) -> tentaflow_sdk_spec::protocol::value::Value {
    use tentaflow_sdk_spec::protocol::value::Value;
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Value::U64(u)
            } else if let Some(i) = n.as_i64() {
                Value::I64(i)
            } else {
                Value::F64(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Value::Text(s),
        serde_json::Value::Array(arr) => {
            Value::Array(arr.into_iter().map(json_value_to_cbor_value).collect())
        }
        serde_json::Value::Object(obj) => {
            let entries: Vec<(Value, Value)> = obj
                .into_iter()
                .map(|(k, v)| (Value::Text(k), json_value_to_cbor_value(v)))
                .collect();
            Value::Map(entries)
        }
    }
}

/// Generate 16 random bytes for ClientActionId using getrandom.
fn random_16_bytes() -> [u8; 16] {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).expect("Losowanie identyfikatora akcji");
    buf
}

// =============================================================================
// Component CBOR decoder — converts Component wire bytes to the JS shape
// expected by ComponentRenderer: { tag, id, fields, handlers, bind, a11y,
// visibility, test_id }.
// =============================================================================

/// Decode a CBOR-encoded Component into a JS object suitable for ComponentRenderer.
#[wasm_bindgen(js_name = decodeComponentCbor)]
pub fn decode_component_cbor(cbor_bytes: &[u8]) -> Result<JsValue, JsError> {
    use tentaflow_sdk_spec::protocol::ui::component::Component;
    let component: Component = minicbor::decode(cbor_bytes)
        .map_err(|e| JsError::new(&format!("Component CBOR decode: {e}")))?;
    component_to_js(&component).map_err(|e| JsError::new(&e))
}

/// Decode CBOR-encoded Vec<StateEntry> into JS array of { path, value }.
#[wasm_bindgen(js_name = decodeStateEntriesCbor)]
pub fn decode_state_entries_cbor(cbor_bytes: &[u8]) -> Result<JsValue, JsError> {
    use tentaflow_sdk_spec::protocol::ui::bind::PathSegment;
    use tentaflow_sdk_spec::protocol::ui::slot::StateEntry;

    let entries: Vec<StateEntry> = minicbor::decode(cbor_bytes)
        .map_err(|e| JsError::new(&format!("StateEntries CBOR decode: {e}")))?;

    let arr = js_sys::Array::new();
    for entry in &entries {
        let obj = js_sys::Object::new();
        let path_arr = js_sys::Array::new();
        for seg in &entry.path.segments {
            let seg_obj = js_sys::Object::new();
            match seg {
                PathSegment::Key(k) => {
                    set(&seg_obj, "kind", "key".into());
                    set(&seg_obj, "value", k.as_str().into());
                }
                PathSegment::Index(i) => {
                    set(&seg_obj, "kind", "index".into());
                    set(&seg_obj, "value", i.clone().into());
                }
            }
            path_arr.push(&seg_obj.into());
        }
        set(&obj, "path", path_arr.into());
        set(
            &obj,
            "value",
            value_to_js(&entry.value).map_err(|e| JsError::new(&e))?,
        );
        arr.push(&obj.into());
    }
    Ok(arr.into())
}

/// Decode CBOR-encoded Vec<PatchOp> into JS array of { path, op, ... }.
#[wasm_bindgen(js_name = decodePatchOpsCbor)]
pub fn decode_patch_ops_cbor(cbor_bytes: &[u8]) -> Result<JsValue, JsError> {
    use tentaflow_sdk_spec::protocol::ui::bind::PathSegment;
    use tentaflow_sdk_spec::protocol::ui::patch::{PatchOp, PatchOpKind};

    let ops: Vec<PatchOp> = minicbor::decode(cbor_bytes)
        .map_err(|e| JsError::new(&format!("PatchOps CBOR decode: {e}")))?;

    let arr = js_sys::Array::new();
    for op in &ops {
        let obj = js_sys::Object::new();
        let path_arr = js_sys::Array::new();
        for seg in &op.path.segments {
            let seg_obj = js_sys::Object::new();
            match seg {
                PathSegment::Key(k) => {
                    set(&seg_obj, "kind", "key".into());
                    set(&seg_obj, "value", k.as_str().into());
                }
                PathSegment::Index(i) => {
                    set(&seg_obj, "kind", "index".into());
                    set(&seg_obj, "value", i.clone().into());
                }
            }
            path_arr.push(&seg_obj.into());
        }
        set(&obj, "path", path_arr.into());
        match &op.op {
            PatchOpKind::Set { value } => {
                set(&obj, "op", "set".into());
                set(
                    &obj,
                    "value",
                    value_to_js(value).map_err(|e| JsError::new(&e))?,
                );
            }
            PatchOpKind::Delete => {
                set(&obj, "op", "delete".into());
            }
            PatchOpKind::AppendArray { value } => {
                set(&obj, "op", "append_array".into());
                set(
                    &obj,
                    "value",
                    value_to_js(value).map_err(|e| JsError::new(&e))?,
                );
            }
            PatchOpKind::PrependArray { value } => {
                set(&obj, "op", "prepend_array".into());
                set(
                    &obj,
                    "value",
                    value_to_js(value).map_err(|e| JsError::new(&e))?,
                );
            }
            PatchOpKind::InsertArray { index, value } => {
                set(&obj, "op", "insert_array".into());
                set(&obj, "index", index.clone().into());
                set(
                    &obj,
                    "value",
                    value_to_js(value).map_err(|e| JsError::new(&e))?,
                );
            }
            PatchOpKind::RemoveArray { index } => {
                set(&obj, "op", "remove_array".into());
                set(&obj, "index", index.clone().into());
            }
            PatchOpKind::MergeMap { value } => {
                set(&obj, "op", "merge_map".into());
                let m_obj = js_sys::Object::new();
                for (k, v) in &value.0 {
                    set(&m_obj, k, value_to_js(v).map_err(|e| JsError::new(&e))?);
                }
                set(&obj, "value", m_obj.into());
            }
            PatchOpKind::Increment { delta } => {
                set(&obj, "op", "increment".into());
                set(&obj, "delta", delta.clone().into());
            }
        }
        arr.push(&obj.into());
    }
    Ok(arr.into())
}

/// Look up the ComponentMeta for a given tag from the schema catalog.
fn component_meta_for_tag(tag: u16) -> Option<&'static tentaflow_sdk_spec::ComponentMeta> {
    static MAP: OnceLock<HashMap<u16, &'static tentaflow_sdk_spec::ComponentMeta>> =
        OnceLock::new();
    let map = MAP.get_or_init(|| {
        tentaflow_sdk_spec::ALL_COMPONENTS
            .iter()
            .map(|m| (m.tag, *m))
            .collect()
    });
    map.get(&tag).copied()
}

/// Look up an InlineMeta by name from the catalog.
fn inline_meta_by_name(name: &str) -> Option<&'static tentaflow_sdk_spec::InlineMeta> {
    static MAP: OnceLock<HashMap<&'static str, &'static tentaflow_sdk_spec::InlineMeta>> =
        OnceLock::new();
    let map = MAP.get_or_init(|| {
        tentaflow_sdk_spec::ALL_INLINE_STRUCTS
            .iter()
            .map(|m| (m.name, *m))
            .collect()
    });
    map.get(name).copied()
}

/// Extract the inline struct name from a wire type string like "Inline<NavTab>"
/// or "Array<Inline<NavTab>>". Returns None if not an inline type.
fn extract_inline_name(wire: &str) -> Option<&str> {
    // "Inline<NavTab>" → "NavTab"
    // "Array<Inline<NavTab>>" → "NavTab"
    // "Option<Inline<IconRef>>" → "IconRef"
    let start = wire.find("Inline<")?;
    let after = &wire[start + 7..];
    let end = after.find('>')?;
    Some(&after[..end])
}

/// Decode a Value::Map using a known InlineMeta to produce FieldMap format:
/// JS Array of `[u8_key, value]` pairs — same shape as Component.fields.
/// Renderers access fields via `ctx.readField(item, integerKey)`.
fn inline_value_to_js(
    entries: &[(
        tentaflow_sdk_spec::protocol::value::Value,
        tentaflow_sdk_spec::protocol::value::Value,
    )],
    meta: &tentaflow_sdk_spec::InlineMeta,
) -> Result<JsValue, String> {
    use tentaflow_sdk_spec::protocol::value::Value;
    let arr = js_sys::Array::new();
    for (k, val) in entries {
        let key_idx = match k {
            Value::U64(n) => *n as u8,
            Value::I64(n) => *n as u8,
            _ => continue,
        };
        let field_wire = meta
            .fields
            .iter()
            .find(|f| f.key == key_idx)
            .map(|f| f.wire)
            .unwrap_or("");
        let js_val = value_to_js_with_wire(val, field_wire)?;
        let pair = js_sys::Array::new();
        pair.push(&key_idx.clone().into());
        pair.push(&js_val);
        arr.push(&pair.into());
    }
    Ok(arr.into())
}

/// True when a wire type denotes an embedded Component: the bare `Component`,
/// any `ComponentRef<...>` (incl. unions) and those wrapped in a single
/// `Option<...>`. Fields like Table.empty_state (`Option<ComponentRef<EmptyState>>`)
/// and row_actions inner (`ComponentRef<Button>`) encode the full child Component,
/// so they must be decoded to a `{tag, id, fields}` object, not a numeric-keyed map.
fn wire_is_component(wire: &str) -> bool {
    let w = wire.trim();
    let w = w
        .strip_prefix("Option<")
        .and_then(|s| s.strip_suffix('>'))
        .map(str::trim)
        .unwrap_or(w);
    w == "Component" || w.starts_with("ComponentRef<") || w.starts_with("Component<")
}

/// Like value_to_js but with wire-type context for inline struct resolution.
fn value_to_js_with_wire(
    v: &tentaflow_sdk_spec::protocol::value::Value,
    wire: &str,
) -> Result<JsValue, String> {
    use tentaflow_sdk_spec::protocol::value::Value;
    // A present field carries no `Option` on the wire, but its declared wire
    // type still does (e.g. `Option<Array<Inline<ResponsiveRule>>>`). Strip the
    // outer `Option<…>` so the `Array<…>` / `Inline<…>` matching below sees the
    // real container type; without this an optional list-of-inline decodes its
    // elements as plain objects instead of FieldMaps.
    let wire = wire
        .strip_prefix("Option<")
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(wire);
    match v {
        // Bytes with a Component wire → decode child Component.
        // Without wire context bytes stay as Uint8Array (no speculative decode).
        Value::Bytes(b) if wire_is_component(wire) => {
            if let Ok(comp) = minicbor::decode::<tentaflow_sdk_spec::Component>(b) {
                return component_to_js(&comp);
            }
            Ok(js_sys::Uint8Array::from(&b[..]).into())
        }
        Value::Array(items) => {
            let arr = js_sys::Array::new();
            let inner_wire = if wire.starts_with("Array<") && wire.ends_with('>') {
                &wire[6..wire.len() - 1]
            } else {
                ""
            };
            for item in items {
                arr.push(&value_to_js_with_wire(item, inner_wire)?);
            }
            Ok(arr.into())
        }
        Value::Map(entries)
            if entries
                .iter()
                .any(|(k, _)| matches!(k, Value::U64(_) | Value::I64(_))) =>
        {
            // Integer-keyed map — try inline struct resolution via wire type context
            if let Some(inline_name) = extract_inline_name(wire) {
                if let Some(meta) = inline_meta_by_name(inline_name) {
                    return inline_value_to_js(entries, meta);
                }
            }
            // Only attempt Component decode when wire context says so
            if wire_is_component(wire) {
                if let Some(comp) = try_decode_component_from_value(v) {
                    return component_to_js(&comp);
                }
            }
            // Fallback: numeric string keys
            let obj = js_sys::Object::new();
            for (k, val) in entries {
                let key_str = match k {
                    Value::U64(n) => n.to_string(),
                    Value::I64(n) => n.to_string(),
                    _ => format!("{k:?}"),
                };
                set(&obj, &key_str, value_to_js_with_wire(val, "")?);
            }
            Ok(obj.into())
        }
        _ => value_to_js(v),
    }
}

fn component_to_js(
    c: &tentaflow_sdk_spec::protocol::ui::component::Component,
) -> Result<JsValue, String> {
    let obj = js_sys::Object::new();
    set(&obj, "tag", c.tag.clone().into());
    set(&obj, "id", c.id.clone().into());

    // fields: Array<[u8, Value]> — use schema-aware conversion so inline
    // structs within fields get text keys instead of integer keys.
    let comp_meta = component_meta_for_tag(c.tag);
    let fields_arr = js_sys::Array::new();
    for (k, v) in &c.fields.0 {
        let wire = comp_meta
            .and_then(|m| m.fields.iter().find(|f| f.key == *k))
            .map(|f| f.wire)
            .unwrap_or("");
        let pair = js_sys::Array::new();
        pair.push(&k.clone().into());
        pair.push(&value_to_js_with_wire(v, wire)?);
        fields_arr.push(&pair.into());
    }
    set(&obj, "fields", fields_arr.into());

    // handlers: Array<[EventKind(string), Handler(object)]> | null
    match &c.handlers {
        Some(hm) => {
            let handlers_arr = js_sys::Array::new();
            for (ek, h) in &hm.0 {
                let pair = js_sys::Array::new();
                pair.push(&ek.as_str().into());
                pair.push(&handler_to_js(h)?);
                handlers_arr.push(&pair.into());
            }
            set(&obj, "handlers", handlers_arr.into());
        }
        None => {
            set(&obj, "handlers", JsValue::NULL);
        }
    }

    // bind: BindSpec object | null
    match &c.bind {
        Some(bs) => set(&obj, "bind", bind_spec_to_js(bs)?),
        None => set(&obj, "bind", JsValue::NULL),
    }

    // a11y: object | null
    match &c.a11y {
        Some(a) => set(&obj, "a11y", accessibility_to_js(a)?),
        None => set(&obj, "a11y", JsValue::NULL),
    }

    // visibility: object | null
    match &c.visibility {
        Some(v) => set(&obj, "visibility", visibility_to_js(v)?),
        None => set(&obj, "visibility", JsValue::NULL),
    }

    // test_id: string | null
    match &c.test_id {
        Some(tid) => set(&obj, "test_id", tid.as_str().into()),
        None => set(&obj, "test_id", JsValue::NULL),
    }

    Ok(obj.into())
}

fn value_to_js(v: &tentaflow_sdk_spec::protocol::value::Value) -> Result<JsValue, String> {
    use tentaflow_sdk_spec::protocol::value::Value;

    match v {
        Value::Null => Ok(JsValue::NULL),
        Value::Bool(b) => Ok((*b).into()),
        Value::U64(n) => Ok(n.clone().into()),
        Value::I64(n) => Ok(n.clone().into()),
        Value::F64(f) => Ok((*f).into()),
        Value::Bytes(b) => {
            // Without wire-type context, bytes are opaque binary data.
            // Component decode only happens in value_to_js_with_wire()
            // when wire == "Component".
            Ok(js_sys::Uint8Array::from(&b[..]).into())
        }
        Value::Text(s) => Ok(s.as_str().into()),
        Value::Array(items) => {
            let arr = js_sys::Array::new();
            for item in items {
                arr.push(&value_to_js(item)?);
            }
            Ok(arr.into())
        }
        Value::Map(entries) => {
            // Without wire-type context, maps are plain JS objects.
            // Component decode only happens in value_to_js_with_wire()
            // when wire == "Component".
            if entries.iter().all(|(k, _)| matches!(k, Value::Text(_))) {
                let obj = js_sys::Object::new();
                for (k, val) in entries {
                    if let Value::Text(key) = k {
                        set(&obj, key, value_to_js(val)?);
                    }
                }
                Ok(obj.into())
            } else {
                // Integer-keyed map without wire-type context — fallback to
                // numeric string keys. Context-aware decoding via
                // value_to_js_with_wire handles inline structs properly.
                let obj = js_sys::Object::new();
                for (k, val) in entries {
                    let key_str = match k {
                        Value::U64(n) => n.to_string(),
                        Value::I64(n) => n.to_string(),
                        _ => format!("{k:?}"),
                    };
                    set(&obj, &key_str, value_to_js(val)?);
                }
                Ok(obj.into())
            }
        }
    }
}

/// Attempt to decode a Value as a Component (for embedded children in FieldMap).
fn try_decode_component_from_value(
    v: &tentaflow_sdk_spec::protocol::value::Value,
) -> Option<tentaflow_sdk_spec::protocol::ui::component::Component> {
    use tentaflow_sdk_spec::protocol::ui::typed_field::decode_from_value;
    decode_from_value(v).ok()
}

/// Encode a minicbor-Encode value to CBOR, decode as generic Value, then to JS.
/// Used for complex sub-structures (Handler, BindSpec, etc.) where hand-coding
/// every variant is impractical.
fn encode_decode_to_js<T: minicbor::Encode<()>>(v: &T) -> Result<JsValue, String> {
    let mut buf = Vec::new();
    minicbor::encode(v, &mut buf).map_err(|e| format!("encode_decode_to_js encode: {e}"))?;
    let val: tentaflow_sdk_spec::protocol::value::Value =
        minicbor::decode(&buf).map_err(|e| format!("encode_decode_to_js decode: {e}"))?;
    value_to_js(&val)
}

fn handler_to_js(
    h: &tentaflow_sdk_spec::protocol::ui::handler::Handler,
) -> Result<JsValue, String> {
    use tentaflow_sdk_spec::protocol::ui::handler::Handler;
    let obj = js_sys::Object::new();
    match h {
        Handler::Local(action) => {
            set(&obj, "kind", "local".into());
            set(&obj, "action", local_action_to_js(action)?);
        }
        Handler::Backend {
            action_id,
            params,
            optimistic,
            on_failure,
        } => {
            set(&obj, "kind", "backend".into());
            set(&obj, "action_id", action_id.as_str().into());
            set(&obj, "params", cbor_map_to_js(params)?);
            if let Some(ops) = optimistic {
                set(&obj, "optimistic", patch_ops_to_js_array(ops)?);
            }
            set(&obj, "on_failure", failure_policy_to_js(on_failure)?);
        }
        Handler::Both {
            action_id,
            params,
            optimistic,
            on_failure,
        } => {
            set(&obj, "kind", "both".into());
            set(&obj, "action_id", action_id.as_str().into());
            set(&obj, "params", cbor_map_to_js(params)?);
            set(&obj, "optimistic", patch_ops_to_js_array(optimistic)?);
            set(&obj, "on_failure", failure_policy_to_js(on_failure)?);
        }
    }
    Ok(obj.into())
}

fn local_action_to_js(
    a: &tentaflow_sdk_spec::protocol::ui::handler::LocalAction,
) -> Result<JsValue, String> {
    use tentaflow_sdk_spec::protocol::ui::handler::LocalAction;
    let obj = js_sys::Object::new();
    match a {
        LocalAction::ShowModal { slot_id } => {
            set(&obj, "kind", "show_modal".into());
            set(&obj, "slot_id", slot_id.as_str().into());
        }
        LocalAction::HideModal { slot_id } => {
            set(&obj, "kind", "hide_modal".into());
            set(&obj, "slot_id", slot_id.as_str().into());
        }
        LocalAction::ToggleSlot { slot_id } => {
            set(&obj, "kind", "toggle_slot".into());
            set(&obj, "slot_id", slot_id.as_str().into());
        }
        LocalAction::SetState { path, value } => {
            set(&obj, "kind", "set_state".into());
            set(&obj, "path", state_path_to_js(path)?);
            set(&obj, "value", value_to_js(value)?);
        }
        LocalAction::DeleteState { path } => {
            set(&obj, "kind", "delete_state".into());
            set(&obj, "path", state_path_to_js(path)?);
        }
        LocalAction::Toggle { path } => {
            set(&obj, "kind", "toggle".into());
            set(&obj, "path", state_path_to_js(path)?);
        }
        LocalAction::Increment { path, delta } => {
            set(&obj, "kind", "increment".into());
            set(&obj, "path", state_path_to_js(path)?);
            set(&obj, "delta", delta.clone().into());
        }
        LocalAction::Navigate { panel_id } => {
            set(&obj, "kind", "navigate".into());
            set(&obj, "panel_id", panel_id.as_str().into());
        }
        LocalAction::Focus { component_id } => {
            set(&obj, "kind", "focus".into());
            set(&obj, "component_id", component_id.as_str().into());
        }
        LocalAction::Scroll {
            component_id,
            behavior,
        } => {
            set(&obj, "kind", "scroll".into());
            set(&obj, "component_id", component_id.as_str().into());
            set(&obj, "behavior", behavior.as_str().into());
        }
        LocalAction::Copy { value } => {
            set(&obj, "kind", "copy".into());
            set(&obj, "value", value.as_str().into());
        }
        LocalAction::Confirm {
            title,
            message,
            destructive,
            then,
        } => {
            set(&obj, "kind", "confirm".into());
            set(&obj, "title", title.as_str().into());
            set(&obj, "message", message.as_str().into());
            set(&obj, "destructive", (*destructive).into());
            set(&obj, "then", handler_to_js(then)?);
        }
        LocalAction::Validate {
            field_component_id,
            rules,
            on_invalid,
        } => {
            set(&obj, "kind", "validate".into());
            set(
                &obj,
                "field_component_id",
                field_component_id.as_str().into(),
            );
            let rules_arr = js_sys::Array::new();
            for r in rules {
                rules_arr.push(&encode_decode_to_js(r)?);
            }
            set(&obj, "rules", rules_arr.into());
            set(&obj, "on_invalid", local_action_to_js(on_invalid)?);
        }
        LocalAction::Debounce { ms, then } => {
            set(&obj, "kind", "debounce".into());
            set(&obj, "ms", ms.clone().into());
            set(&obj, "then", handler_to_js(then)?);
        }
        LocalAction::Sequence { steps } => {
            set(&obj, "kind", "sequence".into());
            let steps_arr = js_sys::Array::new();
            for s in steps {
                steps_arr.push(&handler_to_js(s)?);
            }
            set(&obj, "steps", steps_arr.into());
        }
        LocalAction::Conditional {
            when,
            then,
            else_branch,
        } => {
            set(&obj, "kind", "conditional".into());
            set(&obj, "when", state_condition_to_js(when)?);
            set(&obj, "then", handler_to_js(then)?);
            if let Some(eb) = else_branch {
                set(&obj, "else", handler_to_js(eb)?);
            }
        }
        LocalAction::Noop => {
            set(&obj, "kind", "noop".into());
        }
    }
    Ok(obj.into())
}

fn failure_policy_to_js(
    fp: &tentaflow_sdk_spec::protocol::ui::handler::FailurePolicy,
) -> Result<JsValue, String> {
    use tentaflow_sdk_spec::protocol::ui::handler::FailurePolicy;
    let obj = js_sys::Object::new();
    match fp {
        FailurePolicy::Toast => {
            set(&obj, "kind", "toast".into());
        }
        FailurePolicy::RevertOptimistic => {
            set(&obj, "kind", "revert_optimistic".into());
        }
        FailurePolicy::Custom { action } => {
            set(&obj, "kind", "custom".into());
            set(&obj, "action", local_action_to_js(action)?);
        }
    }
    Ok(obj.into())
}

fn slot_default_to_js(
    sd: &tentaflow_sdk_spec::protocol::ui::slot::SlotDefault,
) -> Result<JsValue, String> {
    use tentaflow_sdk_spec::protocol::ui::slot::SlotDefault;
    let obj = js_sys::Object::new();
    match sd {
        SlotDefault::Empty => {
            set(&obj, "kind", "empty".into());
        }
        SlotDefault::Loading => {
            set(&obj, "kind", "loading".into());
        }
        SlotDefault::Static { fragment } => {
            set(&obj, "kind", "static".into());
            set(&obj, "fragment", component_to_js(fragment)?);
        }
    }
    Ok(obj.into())
}

fn cache_policy_to_js(
    cp: &tentaflow_sdk_spec::protocol::ui::slot::CachePolicy,
) -> Result<JsValue, JsError> {
    use tentaflow_sdk_spec::protocol::ui::slot::CachePolicy;
    let obj = js_sys::Object::new();
    match cp {
        CachePolicy::None => {
            set(&obj, "kind", "none".into());
        }
        CachePolicy::OnNavigateBack => {
            set(&obj, "kind", "on_navigate_back".into());
        }
        CachePolicy::TtlSeconds { value } => {
            set(&obj, "kind", "ttl_seconds".into());
            set(&obj, "value", value.clone().into());
        }
    }
    Ok(obj.into())
}

fn slot_visibility_to_js(
    vis: &tentaflow_sdk_spec::protocol::ui::slot::SlotVisibility,
) -> Result<JsValue, JsError> {
    use tentaflow_sdk_spec::protocol::ui::slot::SlotVisibility;
    let obj = js_sys::Object::new();
    match vis {
        SlotVisibility::Always => {
            set(&obj, "kind", "always".into());
        }
        SlotVisibility::Hidden => {
            set(&obj, "kind", "hidden".into());
        }
        SlotVisibility::Conditional { path } => {
            set(&obj, "kind", "conditional".into());
            // JS visibility.conditional expects a StatePath array (same shape
            // as state_path_to_js) so the SlotManager can subscribe to it.
            set(
                &obj,
                "path",
                state_path_to_js(path).map_err(|e| JsError::new(&e))?,
            );
        }
    }
    Ok(obj.into())
}

fn state_path_to_js(
    sp: &tentaflow_sdk_spec::protocol::ui::bind::StatePath,
) -> Result<JsValue, String> {
    let arr = js_sys::Array::new();
    for seg in &sp.segments {
        arr.push(&path_segment_to_js(seg)?);
    }
    Ok(arr.into())
}

fn path_segment_to_js(
    seg: &tentaflow_sdk_spec::protocol::ui::bind::PathSegment,
) -> Result<JsValue, String> {
    use tentaflow_sdk_spec::protocol::ui::bind::PathSegment;
    let obj = js_sys::Object::new();
    match seg {
        PathSegment::Key(s) => {
            set(&obj, "kind", "key".into());
            set(&obj, "value", s.as_str().into());
        }
        PathSegment::Index(i) => {
            set(&obj, "kind", "index".into());
            set(&obj, "value", i.clone().into());
        }
    }
    Ok(obj.into())
}

fn state_condition_to_js(
    sc: &tentaflow_sdk_spec::protocol::ui::validation::StateCondition,
) -> Result<JsValue, String> {
    use tentaflow_sdk_spec::protocol::ui::validation::StateCondition;
    let obj = js_sys::Object::new();
    match sc {
        StateCondition::IsTruthy { path } => {
            set(&obj, "kind", "is_truthy".into());
            set(&obj, "path", state_path_to_js(path)?);
        }
        StateCondition::IsFalsy { path } => {
            set(&obj, "kind", "is_falsy".into());
            set(&obj, "path", state_path_to_js(path)?);
        }
        StateCondition::Equals { path, value } => {
            set(&obj, "kind", "equals".into());
            set(&obj, "path", state_path_to_js(path)?);
            set(&obj, "value", value_to_js(value)?);
        }
        StateCondition::NotEquals { path, value } => {
            set(&obj, "kind", "not_equals".into());
            set(&obj, "path", state_path_to_js(path)?);
            set(&obj, "value", value_to_js(value)?);
        }
    }
    Ok(obj.into())
}

fn cbor_map_to_js(m: &tentaflow_sdk_spec::protocol::control::CborMap) -> Result<JsValue, String> {
    let obj = js_sys::Object::new();
    for (k, v) in &m.0 {
        set(&obj, k, value_to_js(v)?);
    }
    Ok(obj.into())
}

fn patch_ops_to_js_array(
    ops: &[tentaflow_sdk_spec::protocol::ui::patch::PatchOp],
) -> Result<JsValue, String> {
    let arr = js_sys::Array::new();
    for op in ops {
        arr.push(&encode_decode_to_js(op)?);
    }
    Ok(arr.into())
}

fn bind_spec_to_js(
    bs: &tentaflow_sdk_spec::protocol::ui::bind::BindSpec,
) -> Result<JsValue, String> {
    use tentaflow_sdk_spec::protocol::ui::bind::BindSpec;
    let obj = js_sys::Object::new();
    match bs {
        BindSpec::Text { path, format } => {
            set(&obj, "kind", "text".into());
            set(&obj, "path", state_path_to_js(path)?);
            if let Some(fmt) = format {
                set(&obj, "format", encode_decode_to_js(fmt)?);
            }
        }
        BindSpec::Attr { name, path } => {
            set(&obj, "kind", "attr".into());
            set(&obj, "name", name.as_str().into());
            set(&obj, "path", state_path_to_js(path)?);
        }
        BindSpec::ClassToggle {
            class_name,
            path,
            negate,
        } => {
            set(&obj, "kind", "class_toggle".into());
            set(&obj, "class_name", class_name.as_str().into());
            set(&obj, "path", state_path_to_js(path)?);
            set(&obj, "negate", (*negate).into());
        }
        BindSpec::Show { path, negate } => {
            set(&obj, "kind", "show".into());
            set(&obj, "path", state_path_to_js(path)?);
            set(&obj, "negate", (*negate).into());
        }
        BindSpec::List {
            path,
            item_template_id,
            key_field,
        } => {
            set(&obj, "kind", "list".into());
            set(&obj, "path", state_path_to_js(path)?);
            set(&obj, "item_template_id", item_template_id.as_str().into());
            if let Some(kf) = key_field {
                set(&obj, "key_field", kf.as_str().into());
            }
        }
        BindSpec::TwoWay { path } => {
            set(&obj, "kind", "two_way".into());
            set(&obj, "path", state_path_to_js(path)?);
        }
    }
    Ok(obj.into())
}

fn accessibility_to_js(
    a: &tentaflow_sdk_spec::protocol::ui::a11y::Accessibility,
) -> Result<JsValue, String> {
    let obj = js_sys::Object::new();
    if let Some(r) = &a.role {
        set(&obj, "role", r.as_str().into());
    }
    if let Some(l) = &a.label {
        set(&obj, "label", bind_ref_to_js(l)?);
    }
    if let Some(lf) = &a.label_for {
        set(&obj, "label_for", lf.as_str().into());
    }
    if let Some(db) = &a.described_by {
        set(&obj, "described_by", db.as_str().into());
    }
    if let Some(live) = &a.live {
        set(&obj, "live", live.as_str().into());
    }
    if let Some(exp) = &a.expanded {
        set(&obj, "expanded", bind_ref_to_js(exp)?);
    }
    if let Some(dis) = &a.disabled {
        set(&obj, "disabled", bind_ref_to_js(dis)?);
    }
    if let Some(req) = &a.required {
        set(&obj, "required", bind_ref_to_js(req)?);
    }
    if let Some(inv) = &a.invalid {
        set(&obj, "invalid", bind_ref_to_js(inv)?);
    }
    if let Some(pr) = &a.pressed {
        set(&obj, "pressed", bind_ref_to_js(pr)?);
    }
    if let Some(sel) = &a.selected {
        set(&obj, "selected", bind_ref_to_js(sel)?);
    }
    Ok(obj.into())
}

fn visibility_to_js(
    v: &tentaflow_sdk_spec::protocol::ui::a11y::Visibility,
) -> Result<JsValue, String> {
    let obj = js_sys::Object::new();
    if let Some(vis) = &v.visible {
        set(&obj, "visible", bind_ref_to_js(vis)?);
    }
    if let Some(bp) = &v.display_above_breakpoint {
        set(&obj, "display_above_breakpoint", bp.as_str().into());
    }
    if let Some(bp) = &v.display_below_breakpoint {
        set(&obj, "display_below_breakpoint", bp.as_str().into());
    }
    if v.hidden_for_assistive {
        set(&obj, "hidden_for_assistive", true.into());
    }
    Ok(obj.into())
}

fn bind_ref_to_js(br: &tentaflow_sdk_spec::protocol::ui::bind::BindRef) -> Result<JsValue, String> {
    use tentaflow_sdk_spec::protocol::ui::bind::BindRef;
    let obj = js_sys::Object::new();
    match br {
        BindRef::Literal(v) => {
            set(&obj, "kind", "literal".into());
            set(&obj, "value", value_to_js(v)?);
        }
        BindRef::Bound(path) => {
            set(&obj, "kind", "bound".into());
            set(&obj, "path", state_path_to_js(path)?);
        }
    }
    Ok(obj.into())
}

// ----- Robots core app -----

// ----- Project Studio ("Projekty") -----
// One encoder per ProjectStudioPayload request variant. Requests carrying
// arrays/maps of structs take a JSON string parsed via serde (same pattern as
// encode_benchmark_save_request); simple fields are explicit parameters.
// i64 fields (ActivityList.before_id) cross the JS boundary as Option<String>
// and are parsed here — JS numbers cannot represent the full i64 range.

// ----- Project Studio F2: tests, runs, tasks, generation, notifications -----
// Multi-field mutating requests (CaseSave, RunCreate, TaskSave, ...) take ONE
// snake_case JSON string deserialized straight into the enum variant via its
// external tag — serde enforces field types and applies #[serde(default)], so
// the wasm signature never drifts from the protocol. Simple requests keep
// explicit parameters like the F1 section above.

#[wasm_bindgen]
pub struct ProjectStudioSha256 {
    inner: Option<sha2::Sha256>,
}

#[wasm_bindgen]
impl ProjectStudioSha256 {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Some(sha2::Sha256::default()),
        }
    }

    pub fn update(&mut self, bytes: &[u8]) -> Result<(), JsError> {
        use sha2::Digest;
        self.inner
            .as_mut()
            .ok_or_else(|| JsError::new("hash is finalized"))?
            .update(bytes);
        Ok(())
    }

    #[wasm_bindgen(js_name = digestHex)]
    pub fn digest_hex(&mut self) -> Result<String, JsError> {
        use sha2::Digest;
        let inner = self
            .inner
            .take()
            .ok_or_else(|| JsError::new("hash is finalized"))?;
        Ok(hex::encode(inner.finalize()))
    }
}

// ----- Project Studio F3: test environments, build profiles, runners,
// automated runs, try-run, git/zip/api_spec sources, artifacts, code assist -----
// Same convention as F2: multi-field mutating requests (EnvironmentSave,
// BuildProfileSave, RunStartAuto, TryRunStart) take ONE snake_case JSON string
// parsed by serde; simple requests keep explicit parameters.

// ----- Project Studio F4: run schedules, ML Studio links, kanban status,
// project export/import -----
// Same convention as F2/F3: multi-field mutating requests (ScheduleSave,
// MlProjectCreateFromProject, MlLinkAttach, MlLinkUpdate, ProjectExportStart,
// ProjectImportApply) take ONE snake_case JSON string parsed by serde; simple
// requests keep explicit parameters. Every F4 response decodes through the
// generic ProjectStudio path — none of them carries raw bytes (the export
// archive is fetched over a signed URL, not the binary protocol).

// =============================================================================
// Code Studio — workspace'y, czlonkowie, sesje robocze.
//
// Kazdy wariant ma wlasny, typowany enkoder: argumenty przechodza przez granice
// JS -> WASM jako konkretne typy, a rama idzie na drut jako CBOR
// (`MessageBody::CodeStudioBody`). Jedyne pole podawane jako JSON to lista
// czlonkow w kreatorze — dokladnie tak samo jak `members_json` w Project Studio,
// bo wasm-bindgen nie przenosi `Vec<struct>` bez recznego mostu.
// =============================================================================

// -----------------------------------------------------------------------------
// Code Studio — session surface: filesystem, git broker, patch review, timeline,
// operations, approvals and grants, runs, exec and terminal, settings, index.
//
// Every request here crosses as ONE snake_case JSON string parsed straight into
// the enum variant through serde's external tag. That is the Project Studio F2
// policy (`encode_project_studio_json_request`) and it is the right one for this
// family: fields are append-only with `#[serde(default)]`, so a later field
// needs no change on this side of the bridge, and serde — not a hand-kept
// argument list — enforces the types.
// -----------------------------------------------------------------------------

// =============================================================================
// TentaQuant — the quantum lab app (`MessageBody::TentaQuantBody`). Requests
// are built from a JSON object of their fields, same as TentaNas: the module
// passes the payload it already holds, the enum is deserialized from
// `{ variant: fields }`, and a new variant needs no hand-written encoder.
// Every request but `LabList` carries `instance_id` — the laboratory it means —
// because the package is multi-instance.
//
// `FileUploadChunkRequest` is the one exception: its `bytes` are a Uint8Array,
// which has no place in a JSON object, so it takes typed arguments.
// =============================================================================

// TentaVM — the virtualization app (`MessageBody::TentaVmBody`). Every request
// is built from a JSON object of its fields, same as TentaNas: the dashboard
// passes the payload it already holds, the enum is deserialized from
// `{ variant: fields }`, and new variants need no hand-written encoder.
// `instance_id` is a REQUIRED field of every request — a dashboard that omits
// it fails here instead of addressing the wrong environment.
// =============================================================================
