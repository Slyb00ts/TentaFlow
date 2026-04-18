// =============================================================================
// Plik: tentaflow-protocol-wasm/src/lib.rs
// Opis: WASM bindings dla browser-side rkyv codec. Eksportuje encode/decode
//       dla Envelope + bootstrap MessageBody variants. Bootstrap API zawiera
//       typed helpery dla najczestszych frameow; pelna serde-wasm-bindgen
//       integracja po #27 (proc-macro dispatcher) i #36 (bulk migration).
// Przyklad:
//   import init, {
//     SCHEMA_VERSION, messageKind,
//     encodeEnvelopeDirect, decodeEnvelope,
//     encodeNodeListRequest, encodeMetaHeartbeat, decodeMessageBody,
//   } from './codec.js';
//   await init();
//   const body = encodeNodeListRequest();
//   const frame = encodeEnvelopeDirect(1n, 1, messageKind.META_HEARTBEAT, body);
//   ws.send(frame);
// =============================================================================

use tentaflow_protocol::{
    envelope::{message_kind, Envelope, EnvelopeFlags, Routing},
    message_body::{
        ApiKeyCreateRequest, AuthLoginRequest, ChatMessage, ChatStreamRequest, ClusterUpdateRequest,
        MeshPairInitRequest, MessageBody, ProtocolError, ProtocolErrorCode, SettingEntry,
        SettingsUpdateRequest,
    },
    SCHEMA_VERSION as PROTOCOL_SCHEMA_VERSION,
};
use wasm_bindgen::prelude::*;

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
    sequence: u32,
    message_kind: u16,
    body: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let env = Envelope::new_direct(correlation_id, sequence, message_kind, body);
    rkyv::to_bytes::<rkyv::rancor::Error>(&env)
        .map(|v| v.to_vec())
        .map_err(|e| format!("envelope encode failed: {e}"))
}

/// Buduje Envelope (routing=Direct) z podanymi polami + body bytes; zwraca
/// rkyv-zakodowany frame jako Uint8Array.
///
/// `correlation_id` przekazywany jako u64 (BigInt po stronie JS).
#[wasm_bindgen(js_name = encodeEnvelopeDirect)]
pub fn encode_envelope_direct(
    correlation_id: u64,
    sequence: u32,
    message_kind: u16,
    body: Vec<u8>,
) -> Result<Vec<u8>, JsError> {
    encode_envelope_direct_inner(correlation_id, sequence, message_kind, body)
        .map_err(|e| JsError::new(&e))
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
    pub sequence: u32,
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

    /// Rkyv-zakodowany MessageBody — przekazac do `decodeMessageBody()`.
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
    let env = rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(bytes)
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
    rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(bytes).is_ok()
}

// =============================================================================
// MessageBody encode helpers (bootstrap typed constructors)
// =============================================================================

fn encode_body_inner(body: &MessageBody) -> Result<Vec<u8>, String> {
    rkyv::to_bytes::<rkyv::rancor::Error>(body)
        .map(|v| v.to_vec())
        .map_err(|e| format!("body encode failed: {e}"))
}

fn encode_node_info_request_inner(node_id: &[u8]) -> Result<Vec<u8>, String> {
    if node_id.len() != 32 {
        return Err("node_id must be exactly 32 bytes".to_string());
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(node_id);
    encode_body_inner(&MessageBody::NodeInfoRequest { node_id: buf })
}

/// MessageBody::NodeListRequest (unit variant).
#[wasm_bindgen(js_name = encodeNodeListRequest)]
pub fn encode_node_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::NodeListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::ModelListRequest (unit variant).
#[wasm_bindgen(js_name = encodeModelListRequest)]
pub fn encode_model_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::MetaHeartbeat { sent_at_epoch }.
#[wasm_bindgen(js_name = encodeMetaHeartbeat)]
pub fn encode_meta_heartbeat(sent_at_epoch: u64) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MetaHeartbeat { sent_at_epoch }).map_err(|e| JsError::new(&e))
}

/// MessageBody::MetaCancelStream (unit variant). Correlation_id idzie w envelope.
#[wasm_bindgen(js_name = encodeMetaCancelStream)]
pub fn encode_meta_cancel_stream() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MetaCancelStream).map_err(|e| JsError::new(&e))
}

/// MessageBody::MetaSchemaVersionCheck { client_version }.
/// Wysylane raz przy handshake — jesli serwer odrzuci, disconnect.
#[wasm_bindgen(js_name = encodeMetaSchemaVersionCheck)]
pub fn encode_meta_schema_version_check(client_version: u16) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MetaSchemaVersionCheck { client_version })
        .map_err(|e| JsError::new(&e))
}

/// MessageBody::NodeInfoRequest { node_id }. node_id MUSI byc 32 bajtami.
#[wasm_bindgen(js_name = encodeNodeInfoRequest)]
pub fn encode_node_info_request(node_id: &[u8]) -> Result<Vec<u8>, JsError> {
    encode_node_info_request_inner(node_id).map_err(|e| JsError::new(&e))
}

/// MessageBody::ApiKeyListRequest (unit variant).
#[wasm_bindgen(js_name = encodeApiKeyListRequest)]
pub fn encode_api_key_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ApiKeyListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::ApiKeyCreateRequest { name, scopes }.
#[wasm_bindgen(js_name = encodeApiKeyCreateRequest)]
pub fn encode_api_key_create_request(name: String, scopes: Vec<String>) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ApiKeyCreateRequestBody(ApiKeyCreateRequest {
        name,
        scopes,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ApiKeyRevokeRequest { key_id }.
#[wasm_bindgen(js_name = encodeApiKeyRevokeRequest)]
pub fn encode_api_key_revoke_request(key_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ApiKeyRevokeRequest { key_id }).map_err(|e| JsError::new(&e))
}

/// MessageBody::AuthLoginRequest { username, password }.
#[wasm_bindgen(js_name = encodeAuthLoginRequest)]
pub fn encode_auth_login_request(username: String, password: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AuthLoginRequestBody(AuthLoginRequest {
        username,
        password,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AuthMeRequest (unit variant).
#[wasm_bindgen(js_name = encodeAuthMeRequest)]
pub fn encode_auth_me_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AuthMeRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::ChatStreamRequest — przyjmuje JSON string messages, parsuje
/// jako JsValue. Bootstrap accepts tylko `model_id` + jednoelementowa lista
/// user messages. Pelny messages[] input po integracji serde-wasm-bindgen (#36 ph.2).
#[wasm_bindgen(js_name = encodeChatStreamRequestSimple)]
pub fn encode_chat_stream_request_simple(
    model_id: String,
    user_message: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ChatStreamRequestBody(ChatStreamRequest {
        model_id,
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: user_message,
        }],
        temperature: None,
        max_tokens: None,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterUpdateRequest.
#[wasm_bindgen(js_name = encodeClusterUpdateRequest)]
pub fn encode_cluster_update_request(
    cluster_id: String,
    name: String,
    description: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterUpdateRequestBody(ClusterUpdateRequest {
        cluster_id,
        name,
        description,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::MeshPeersListRequest (unit variant).
#[wasm_bindgen(js_name = encodeMeshPeersListRequest)]
pub fn encode_mesh_peers_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshPeersListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::MeshPairInitRequest { node_id (32 bytes), pin }.
#[wasm_bindgen(js_name = encodeMeshPairInitRequest)]
pub fn encode_mesh_pair_init_request(node_id: &[u8], pin: String) -> Result<Vec<u8>, JsError> {
    if node_id.len() != 32 {
        return Err(JsError::new("node_id must be exactly 32 bytes"));
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(node_id);
    encode_body_inner(&MessageBody::MeshPairInitRequestBody(MeshPairInitRequest {
        node_id: buf,
        pin,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::SettingsListRequest (unit variant).
#[wasm_bindgen(js_name = encodeSettingsListRequest)]
pub fn encode_settings_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SettingsListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::SettingsUpdateRequest — simplified: para key/value/is_secret.
/// Pelna lista (N elementow) po integracji serde-wasm-bindgen (#36 phase 2).
#[wasm_bindgen(js_name = encodeSettingsUpdateSingle)]
pub fn encode_settings_update_single(
    key: String,
    value: String,
    is_secret: bool,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SettingsUpdateRequestBody(SettingsUpdateRequest {
        entries: vec![SettingEntry {
            key,
            value,
            is_secret,
        }],
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::DashboardMetricsRequest (unit variant).
#[wasm_bindgen(js_name = encodeDashboardMetricsRequest)]
pub fn encode_dashboard_metrics_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::DashboardMetricsRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::SubscribeResumeRequest { resume_token }.
/// Klient po reconnect przekazuje token z poprzedniej SubscribeResumeOffer.
#[wasm_bindgen(js_name = encodeSubscribeResumeRequest)]
pub fn encode_subscribe_resume_request(resume_token: Vec<u8>) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SubscribeResumeRequest { resume_token })
        .map_err(|e| JsError::new(&e))
}

// =============================================================================
// MessageBody decode (zwraca JS object z variant tag + polami)
// =============================================================================

fn set(obj: &js_sys::Object, key: &str, value: JsValue) {
    let _ = js_sys::Reflect::set(obj, &key.into(), &value);
}

/// Dekoduje rkyv-zakodowany MessageBody na JS object w formacie
/// `{ variant: "NodeListResponse", nodes: [...] }`. Dla bootstrap variantow
/// pokrywa 10 kejsow; nieznany variant zwraca `{ variant: "Unknown" }`.
#[wasm_bindgen(js_name = decodeMessageBody)]
pub fn decode_message_body(bytes: &[u8]) -> Result<JsValue, JsError> {
    let body = rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(bytes)
        .map_err(|e| JsError::new(&format!("body decode failed: {e}")))?;

    let obj = js_sys::Object::new();
    match body {
        MessageBody::MetaSchemaVersionCheck { client_version } => {
            set(&obj, "variant", "MetaSchemaVersionCheck".into());
            set(&obj, "clientVersion", (client_version as u32).into());
        }
        MessageBody::MetaSchemaVersionAck {
            server_version,
            accepted,
        } => {
            set(&obj, "variant", "MetaSchemaVersionAck".into());
            set(&obj, "serverVersion", (server_version as u32).into());
            set(&obj, "accepted", accepted.into());
        }
        MessageBody::MetaHeartbeat { sent_at_epoch } => {
            set(&obj, "variant", "MetaHeartbeat".into());
            set(&obj, "sentAtEpoch", sent_at_epoch.into());
        }
        MessageBody::MetaCancelStream => {
            set(&obj, "variant", "MetaCancelStream".into());
        }
        MessageBody::NodeListRequest => {
            set(&obj, "variant", "NodeListRequest".into());
        }
        MessageBody::NodeListResponse { nodes } => {
            set(&obj, "variant", "NodeListResponse".into());
            let arr = js_sys::Array::new();
            for n in nodes {
                let item = js_sys::Object::new();
                set(&item, "nodeId", js_sys::Uint8Array::from(&n.node_id[..]).into());
                set(&item, "displayName", n.display_name.into());
                set(&item, "status", n.status.into());
                set(&item, "role", n.role.into());
                set(&item, "isSelf", n.is_self.into());
                arr.push(&item.into());
            }
            set(&obj, "nodes", arr.into());
        }
        MessageBody::ModelListRequest => {
            set(&obj, "variant", "ModelListRequest".into());
        }
        MessageBody::ModelListResponse { models } => {
            set(&obj, "variant", "ModelListResponse".into());
            let arr = js_sys::Array::new();
            for m in models {
                let item = js_sys::Object::new();
                set(&item, "id", m.id.into());
                set(&item, "category", m.category.into());
                set(&item, "engineId", m.engine_id.into());
                set(&item, "availability", m.availability.into());
                arr.push(&item.into());
            }
            set(&obj, "models", arr.into());
        }
        MessageBody::NodeInfoRequest { node_id } => {
            set(&obj, "variant", "NodeInfoRequest".into());
            set(&obj, "nodeId", js_sys::Uint8Array::from(&node_id[..]).into());
        }
        MessageBody::ApiKeyListRequest => {
            set(&obj, "variant", "ApiKeyListRequest".into());
        }
        MessageBody::ApiKeyListResponse { keys } => {
            set(&obj, "variant", "ApiKeyListResponse".into());
            let arr = js_sys::Array::new();
            for k in keys {
                let item = js_sys::Object::new();
                set(&item, "keyId", k.key_id.into());
                set(&item, "name", k.name.into());
                set(&item, "createdAtEpoch", k.created_at_epoch.into());
                if let Some(used) = k.last_used_at_epoch {
                    set(&item, "lastUsedAtEpoch", used.into());
                }
                arr.push(&item.into());
            }
            set(&obj, "keys", arr.into());
        }
        MessageBody::ApiKeyCreateRequestBody(req) => {
            set(&obj, "variant", "ApiKeyCreateRequest".into());
            set(&obj, "name", req.name.into());
            let scopes_arr = js_sys::Array::new();
            for s in req.scopes {
                scopes_arr.push(&JsValue::from_str(&s));
            }
            set(&obj, "scopes", scopes_arr.into());
        }
        MessageBody::ApiKeyCreateResponseBody(resp) => {
            set(&obj, "variant", "ApiKeyCreateResponse".into());
            set(&obj, "keyId", resp.key_id.into());
            set(&obj, "token", resp.token.into());
        }
        MessageBody::ApiKeyRevokeRequest { key_id } => {
            set(&obj, "variant", "ApiKeyRevokeRequest".into());
            set(&obj, "keyId", key_id.into());
        }
        MessageBody::ApiKeyRevokeResponse { deleted } => {
            set(&obj, "variant", "ApiKeyRevokeResponse".into());
            set(&obj, "deleted", deleted.into());
        }
        MessageBody::AuthLoginRequestBody(req) => {
            set(&obj, "variant", "AuthLoginRequest".into());
            set(&obj, "username", req.username.into());
            // password NIGDY nie odslaniamy w response logu
            set(&obj, "password", "<redacted>".into());
        }
        MessageBody::AuthLoginResponseBody(resp) => {
            set(&obj, "variant", "AuthLoginResponse".into());
            set(&obj, "jwt", resp.jwt.into());
            set(&obj, "userId", js_sys::Uint8Array::from(&resp.user_id[..]).into());
            set(&obj, "role", resp.role.into());
        }
        MessageBody::AuthMeRequest => {
            set(&obj, "variant", "AuthMeRequest".into());
        }
        MessageBody::AuthMeResponseBody(resp) => {
            set(&obj, "variant", "AuthMeResponse".into());
            set(&obj, "userId", js_sys::Uint8Array::from(&resp.user_id[..]).into());
            set(&obj, "username", resp.username.into());
            set(&obj, "role", resp.role.into());
        }
        MessageBody::ChatStreamRequestBody(req) => {
            set(&obj, "variant", "ChatStreamRequest".into());
            set(&obj, "modelId", req.model_id.into());
            let messages_arr = js_sys::Array::new();
            for m in req.messages {
                let item = js_sys::Object::new();
                set(&item, "role", m.role.into());
                set(&item, "content", m.content.into());
                messages_arr.push(&item.into());
            }
            set(&obj, "messages", messages_arr.into());
        }
        MessageBody::ChatStreamChunkBody(chunk) => {
            set(&obj, "variant", "ChatStreamChunk".into());
            set(&obj, "delta", chunk.delta.into());
        }
        MessageBody::ChatStreamEndBody(end) => {
            set(&obj, "variant", "ChatStreamEnd".into());
            set(&obj, "promptTokens", (end.prompt_tokens as u32).into());
            set(&obj, "completionTokens", (end.completion_tokens as u32).into());
        }
        MessageBody::ClusterUpdateRequestBody(req) => {
            set(&obj, "variant", "ClusterUpdateRequest".into());
            set(&obj, "clusterId", req.cluster_id.into());
            set(&obj, "name", req.name.into());
            if let Some(d) = req.description {
                set(&obj, "description", d.into());
            }
        }
        MessageBody::ClusterUpdateResponseBody(resp) => {
            set(&obj, "variant", "ClusterUpdateResponse".into());
            set(&obj, "clusterId", resp.cluster_id.into());
            set(&obj, "updatedAtEpoch", resp.updated_at_epoch.into());
        }
        MessageBody::MeshTrustRevoked(evt) => {
            set(&obj, "variant", "MeshTrustRevoked".into());
            set(
                &obj,
                "revokedNodeId",
                js_sys::Uint8Array::from(&evt.revoked_node_id[..]).into(),
            );
            set(&obj, "reason", evt.reason.into());
            set(&obj, "revokedAtEpoch", evt.revoked_at_epoch.into());
        }
        MessageBody::MeshTrustedKeysSync(evt) => {
            set(&obj, "variant", "MeshTrustedKeysSync".into());
            let arr = js_sys::Array::new();
            for k in evt.trusted_keys {
                arr.push(&js_sys::Uint8Array::from(&k[..]).into());
            }
            set(&obj, "trustedKeys", arr.into());
            set(&obj, "epoch", (evt.epoch as u32).into());
        }
        MessageBody::SubscribeResumeRequest { resume_token } => {
            set(&obj, "variant", "SubscribeResumeRequest".into());
            set(&obj, "resumeToken", js_sys::Uint8Array::from(&resume_token[..]).into());
        }
        MessageBody::SubscribeResumeAck { accepted, error } => {
            set(&obj, "variant", "SubscribeResumeAck".into());
            set(&obj, "accepted", accepted.into());
            if let Some(err) = error {
                set(&obj, "error", err.into());
            }
        }
        MessageBody::SubscribeResumeOffer { resume_token } => {
            set(&obj, "variant", "SubscribeResumeOffer".into());
            set(&obj, "resumeToken", js_sys::Uint8Array::from(&resume_token[..]).into());
        }
        MessageBody::ModelDetailRequest { model_id } => {
            set(&obj, "variant", "ModelDetailRequest".into());
            set(&obj, "modelId", model_id.into());
        }
        MessageBody::ModelDetailResponse(d) => {
            set(&obj, "variant", "ModelDetailResponse".into());
            set(&obj, "id", d.id.into());
            set(&obj, "category", d.category.into());
            set(&obj, "engineId", d.engine_id.into());
            if let Some(p) = d.local_path {
                set(&obj, "localPath", p.into());
            }
            set(&obj, "sizeBytes", d.size_bytes.into());
            set(&obj, "availability", d.availability.into());
            set(&obj, "description", d.description.into());
            if let Some(c) = d.checksum_sha256 {
                set(&obj, "checksumSha256", c.into());
            }
        }
        MessageBody::ModelInstallRequestBody(req) => {
            set(&obj, "variant", "ModelInstallRequest".into());
            set(&obj, "modelId", req.model_id.into());
            set(&obj, "sourceRepo", req.source_repo.into());
        }
        MessageBody::ModelInstallResponse { model_id, accepted } => {
            set(&obj, "variant", "ModelInstallResponse".into());
            set(&obj, "modelId", model_id.into());
            set(&obj, "accepted", accepted.into());
        }
        MessageBody::ModelDeleteRequest { model_id } => {
            set(&obj, "variant", "ModelDeleteRequest".into());
            set(&obj, "modelId", model_id.into());
        }
        MessageBody::ModelDeleteResponse { deleted } => {
            set(&obj, "variant", "ModelDeleteResponse".into());
            set(&obj, "deleted", deleted.into());
        }
        MessageBody::HubEngineListRequest => {
            set(&obj, "variant", "HubEngineListRequest".into());
        }
        MessageBody::HubEngineListResponse { engines } => {
            set(&obj, "variant", "HubEngineListResponse".into());
            let arr = js_sys::Array::new();
            for e in engines {
                let item = js_sys::Object::new();
                set(&item, "id", e.id.into());
                set(&item, "displayName", e.display_name.into());
                set(&item, "category", e.category.into());
                let methods = js_sys::Array::new();
                for m in e.deploy_methods {
                    methods.push(&JsValue::from_str(&m));
                }
                set(&item, "deployMethods", methods.into());
                set(&item, "defaultPort", (e.default_port as u32).into());
                arr.push(&item.into());
            }
            set(&obj, "engines", arr.into());
        }
        MessageBody::HubModelSearchRequest { query } => {
            set(&obj, "variant", "HubModelSearchRequest".into());
            set(&obj, "query", query.into());
        }
        MessageBody::HubModelSearchResponse { results } => {
            set(&obj, "variant", "HubModelSearchResponse".into());
            let arr = js_sys::Array::new();
            for r in results {
                let item = js_sys::Object::new();
                set(&item, "repoId", r.repo_id.into());
                set(&item, "displayName", r.display_name.into());
                set(&item, "author", r.author.into());
                set(&item, "downloads", r.downloads.into());
                set(&item, "likes", r.likes.into());
                set(&item, "lastModifiedEpoch", r.last_modified_epoch.into());
                arr.push(&item.into());
            }
            set(&obj, "results", arr.into());
        }
        MessageBody::HubDownloadProgressBody(p) => {
            set(&obj, "variant", "HubDownloadProgress".into());
            set(&obj, "modelId", p.model_id.into());
            set(&obj, "bytesDownloaded", p.bytes_downloaded.into());
            set(&obj, "bytesTotal", p.bytes_total.into());
            set(&obj, "speedBps", p.speed_bps.into());
            if let Some(eta) = p.eta_seconds {
                set(&obj, "etaSeconds", eta.into());
            }
        }
        MessageBody::FlowListRequest => {
            set(&obj, "variant", "FlowListRequest".into());
        }
        MessageBody::FlowListResponse { flows } => {
            set(&obj, "variant", "FlowListResponse".into());
            let arr = js_sys::Array::new();
            for f in flows {
                let item = js_sys::Object::new();
                set(&item, "id", f.id.into());
                set(&item, "name", f.name.into());
                if let Some(d) = f.description {
                    set(&item, "description", d.into());
                }
                set(&item, "createdAtEpoch", f.created_at_epoch.into());
                set(&item, "updatedAtEpoch", f.updated_at_epoch.into());
                set(&item, "enabled", f.enabled.into());
                arr.push(&item.into());
            }
            set(&obj, "flows", arr.into());
        }
        MessageBody::FlowDetailRequest { flow_id } => {
            set(&obj, "variant", "FlowDetailRequest".into());
            set(&obj, "flowId", flow_id.into());
        }
        MessageBody::FlowDetailResponse(d) => {
            set(&obj, "variant", "FlowDetailResponse".into());
            set(&obj, "id", d.id.into());
            set(&obj, "name", d.name.into());
            if let Some(desc) = d.description {
                set(&obj, "description", desc.into());
            }
            set(&obj, "graphJson", d.graph_json.into());
            set(&obj, "enabled", d.enabled.into());
        }
        MessageBody::FlowCreateRequestBody(req) => {
            set(&obj, "variant", "FlowCreateRequest".into());
            set(&obj, "name", req.name.into());
            if let Some(d) = req.description {
                set(&obj, "description", d.into());
            }
            set(&obj, "graphJson", req.graph_json.into());
        }
        MessageBody::FlowCreateResponse { flow_id } => {
            set(&obj, "variant", "FlowCreateResponse".into());
            set(&obj, "flowId", flow_id.into());
        }
        MessageBody::FlowDeleteRequest { flow_id } => {
            set(&obj, "variant", "FlowDeleteRequest".into());
            set(&obj, "flowId", flow_id.into());
        }
        MessageBody::FlowDeleteResponse { deleted } => {
            set(&obj, "variant", "FlowDeleteResponse".into());
            set(&obj, "deleted", deleted.into());
        }
        MessageBody::FlowExecutionsListRequest { flow_id } => {
            set(&obj, "variant", "FlowExecutionsListRequest".into());
            set(&obj, "flowId", flow_id.into());
        }
        MessageBody::FlowExecutionsListResponse { executions } => {
            set(&obj, "variant", "FlowExecutionsListResponse".into());
            let arr = js_sys::Array::new();
            for e in executions {
                let item = js_sys::Object::new();
                set(&item, "id", e.id.into());
                set(&item, "flowId", e.flow_id.into());
                set(&item, "status", e.status.into());
                set(&item, "startedAtEpoch", e.started_at_epoch.into());
                if let Some(c) = e.completed_at_epoch {
                    set(&item, "completedAtEpoch", c.into());
                }
                arr.push(&item.into());
            }
            set(&obj, "executions", arr.into());
        }
        MessageBody::ServiceListRequest => {
            set(&obj, "variant", "ServiceListRequest".into());
        }
        MessageBody::ServiceListResponse { services } => {
            set(&obj, "variant", "ServiceListResponse".into());
            let arr = js_sys::Array::new();
            for s in services {
                let item = js_sys::Object::new();
                set(&item, "id", s.id.into());
                set(&item, "engineId", s.engine_id.into());
                set(&item, "modelId", s.model_id.into());
                set(&item, "status", s.status.into());
                set(&item, "deployMethod", s.deploy_method.into());
                if let Some(url) = s.endpoint_url {
                    set(&item, "endpointUrl", url.into());
                }
                if let Some(t) = s.started_at_epoch {
                    set(&item, "startedAtEpoch", t.into());
                }
                arr.push(&item.into());
            }
            set(&obj, "services", arr.into());
        }
        MessageBody::ServiceDeployRequestBody(req) => {
            set(&obj, "variant", "ServiceDeployRequest".into());
            set(&obj, "engineId", req.engine_id.into());
            set(&obj, "modelId", req.model_id.into());
            set(&obj, "deployMethod", req.deploy_method.into());
            set(&obj, "nodeId", js_sys::Uint8Array::from(&req.node_id[..]).into());
        }
        MessageBody::ServiceDeployAccepted { deploy_id } => {
            set(&obj, "variant", "ServiceDeployAccepted".into());
            set(&obj, "deployId", deploy_id.into());
        }
        MessageBody::ServiceDeployProgressBody(p) => {
            set(&obj, "variant", "ServiceDeployProgress".into());
            set(&obj, "deployId", p.deploy_id.into());
            set(&obj, "stage", p.stage.into());
            set(&obj, "progressPercent", (p.progress_percent as u32).into());
            set(&obj, "message", p.message.into());
        }
        MessageBody::ServiceStopRequest { service_id } => {
            set(&obj, "variant", "ServiceStopRequest".into());
            set(&obj, "serviceId", service_id.into());
        }
        MessageBody::ServiceStopResponse { stopped } => {
            set(&obj, "variant", "ServiceStopResponse".into());
            set(&obj, "stopped", stopped.into());
        }
        MessageBody::PromptListRequest => {
            set(&obj, "variant", "PromptListRequest".into());
        }
        MessageBody::PromptListResponse { prompts } => {
            set(&obj, "variant", "PromptListResponse".into());
            let arr = js_sys::Array::new();
            for p in prompts {
                let item = js_sys::Object::new();
                set(&item, "id", p.id.into());
                set(&item, "name", p.name.into());
                set(&item, "category", p.category.into());
                set(&item, "updatedAtEpoch", p.updated_at_epoch.into());
                arr.push(&item.into());
            }
            set(&obj, "prompts", arr.into());
        }
        MessageBody::PromptDetailRequest { prompt_id } => {
            set(&obj, "variant", "PromptDetailRequest".into());
            set(&obj, "promptId", prompt_id.into());
        }
        MessageBody::PromptDetailResponse(d) => {
            set(&obj, "variant", "PromptDetailResponse".into());
            set(&obj, "id", d.id.into());
            set(&obj, "name", d.name.into());
            set(&obj, "category", d.category.into());
            set(&obj, "template", d.template.into());
            let vars = js_sys::Array::new();
            for v in d.variables {
                vars.push(&JsValue::from_str(&v));
            }
            set(&obj, "variables", vars.into());
            set(&obj, "updatedAtEpoch", d.updated_at_epoch.into());
        }
        MessageBody::RegistryListRequest => {
            set(&obj, "variant", "RegistryListRequest".into());
        }
        MessageBody::RegistryListResponse { registries } => {
            set(&obj, "variant", "RegistryListResponse".into());
            let arr = js_sys::Array::new();
            for r in registries {
                let item = js_sys::Object::new();
                set(&item, "id", r.id.into());
                set(&item, "url", r.url.into());
                set(&item, "kind", r.kind.into());
                set(&item, "authRequired", r.auth_required.into());
                arr.push(&item.into());
            }
            set(&obj, "registries", arr.into());
        }
        MessageBody::AuditEventBody(e) => {
            set(&obj, "variant", "AuditEvent".into());
            set(&obj, "tsEpoch", e.ts_epoch.into());
            if let Some(u) = e.user_id {
                set(&obj, "userId", js_sys::Uint8Array::from(&u[..]).into());
            }
            set(&obj, "eventKind", e.event_kind.into());
            if let Some(r) = e.resource_id {
                set(&obj, "resourceId", r.into());
            }
            set(&obj, "message", e.message.into());
        }
        MessageBody::ContainerListRequest => {
            set(&obj, "variant", "ContainerListRequest".into());
        }
        MessageBody::ContainerListResponse { containers } => {
            set(&obj, "variant", "ContainerListResponse".into());
            let arr = js_sys::Array::new();
            for c in containers {
                let item = js_sys::Object::new();
                set(&item, "id", c.id.into());
                set(&item, "name", c.name.into());
                set(&item, "image", c.image.into());
                set(&item, "state", c.state.into());
                set(&item, "createdAtEpoch", c.created_at_epoch.into());
                let ports = js_sys::Array::new();
                for p in c.ports {
                    ports.push(&JsValue::from_str(&p));
                }
                set(&item, "ports", ports.into());
                arr.push(&item.into());
            }
            set(&obj, "containers", arr.into());
        }
        MessageBody::ContainerStartRequest { container_id } => {
            set(&obj, "variant", "ContainerStartRequest".into());
            set(&obj, "containerId", container_id.into());
        }
        MessageBody::ContainerStartResponse { started } => {
            set(&obj, "variant", "ContainerStartResponse".into());
            set(&obj, "started", started.into());
        }
        MessageBody::ContainerStopRequest { container_id } => {
            set(&obj, "variant", "ContainerStopRequest".into());
            set(&obj, "containerId", container_id.into());
        }
        MessageBody::ContainerStopResponse { stopped } => {
            set(&obj, "variant", "ContainerStopResponse".into());
            set(&obj, "stopped", stopped.into());
        }
        MessageBody::ContainerLogStreamRequest { container_id, follow } => {
            set(&obj, "variant", "ContainerLogStreamRequest".into());
            set(&obj, "containerId", container_id.into());
            set(&obj, "follow", follow.into());
        }
        MessageBody::ContainerLogChunkBody(c) => {
            set(&obj, "variant", "ContainerLogChunk".into());
            set(&obj, "containerId", c.container_id.into());
            set(&obj, "stream", c.stream.into());
            set(&obj, "line", c.line.into());
            set(&obj, "tsEpoch", c.ts_epoch.into());
        }
        MessageBody::VoiceProfileListRequest => {
            set(&obj, "variant", "VoiceProfileListRequest".into());
        }
        MessageBody::VoiceProfileListResponse { profiles } => {
            set(&obj, "variant", "VoiceProfileListResponse".into());
            let arr = js_sys::Array::new();
            for p in profiles {
                let item = js_sys::Object::new();
                set(&item, "id", p.id.into());
                set(&item, "displayName", p.display_name.into());
                set(&item, "embeddingCount", (p.embedding_count as u32).into());
                set(&item, "createdAtEpoch", p.created_at_epoch.into());
                arr.push(&item.into());
            }
            set(&obj, "profiles", arr.into());
        }
        MessageBody::TtsRuleListRequest => {
            set(&obj, "variant", "TtsRuleListRequest".into());
        }
        MessageBody::TtsRuleListResponse { rules } => {
            set(&obj, "variant", "TtsRuleListResponse".into());
            let arr = js_sys::Array::new();
            for r in rules {
                let item = js_sys::Object::new();
                set(&item, "id", r.id.into());
                set(&item, "pattern", r.pattern.into());
                set(&item, "voiceId", r.voice_id.into());
                set(&item, "priority", r.priority.into());
                arr.push(&item.into());
            }
            set(&obj, "rules", arr.into());
        }
        MessageBody::TtsRuleCreateRequest(r) => {
            set(&obj, "variant", "TtsRuleCreateRequest".into());
            set(&obj, "id", r.id.into());
            set(&obj, "pattern", r.pattern.into());
            set(&obj, "voiceId", r.voice_id.into());
            set(&obj, "priority", r.priority.into());
        }
        MessageBody::TtsRuleCreateResponse { rule_id } => {
            set(&obj, "variant", "TtsRuleCreateResponse".into());
            set(&obj, "ruleId", rule_id.into());
        }
        MessageBody::TtsRuleDeleteRequest { rule_id } => {
            set(&obj, "variant", "TtsRuleDeleteRequest".into());
            set(&obj, "ruleId", rule_id.into());
        }
        MessageBody::TtsRuleDeleteResponse { deleted } => {
            set(&obj, "variant", "TtsRuleDeleteResponse".into());
            set(&obj, "deleted", deleted.into());
        }
        MessageBody::PiiRuleListRequest => {
            set(&obj, "variant", "PiiRuleListRequest".into());
        }
        MessageBody::PiiRuleListResponse { rules } => {
            set(&obj, "variant", "PiiRuleListResponse".into());
            let arr = js_sys::Array::new();
            for r in rules {
                let item = js_sys::Object::new();
                set(&item, "id", r.id.into());
                set(&item, "kind", r.kind.into());
                set(&item, "regex", r.regex.into());
                set(&item, "action", r.action.into());
                arr.push(&item.into());
            }
            set(&obj, "rules", arr.into());
        }
        MessageBody::FastPathListRequest => {
            set(&obj, "variant", "FastPathListRequest".into());
        }
        MessageBody::FastPathListResponse { patterns } => {
            set(&obj, "variant", "FastPathListResponse".into());
            let arr = js_sys::Array::new();
            for p in patterns {
                let item = js_sys::Object::new();
                set(&item, "id", p.id.into());
                set(&item, "pattern", p.pattern.into());
                set(&item, "response", p.response.into());
                set(&item, "priority", p.priority.into());
                arr.push(&item.into());
            }
            set(&obj, "patterns", arr.into());
        }
        MessageBody::MeshPeersListRequest => {
            set(&obj, "variant", "MeshPeersListRequest".into());
        }
        MessageBody::MeshPeersListResponse { peers } => {
            set(&obj, "variant", "MeshPeersListResponse".into());
            let arr = js_sys::Array::new();
            for p in peers {
                let item = js_sys::Object::new();
                set(&item, "nodeId", js_sys::Uint8Array::from(&p.node_id[..]).into());
                set(&item, "displayName", p.display_name.into());
                set(&item, "trustState", p.trust_state.into());
                if let Some(ep) = p.endpoint {
                    set(&item, "endpoint", ep.into());
                }
                if let Some(ls) = p.last_seen_epoch {
                    set(&item, "lastSeenEpoch", ls.into());
                }
                arr.push(&item.into());
            }
            set(&obj, "peers", arr.into());
        }
        MessageBody::MeshPairInitRequestBody(req) => {
            set(&obj, "variant", "MeshPairInitRequest".into());
            set(&obj, "nodeId", js_sys::Uint8Array::from(&req.node_id[..]).into());
            set(&obj, "pin", req.pin.into());
        }
        MessageBody::MeshPairInitResponseBody(resp) => {
            set(&obj, "variant", "MeshPairInitResponse".into());
            set(&obj, "pairId", resp.pair_id.into());
            set(&obj, "expiresAtEpoch", resp.expires_at_epoch.into());
        }
        MessageBody::SettingsListRequest => {
            set(&obj, "variant", "SettingsListRequest".into());
        }
        MessageBody::SettingsListResponse { entries } => {
            set(&obj, "variant", "SettingsListResponse".into());
            let arr = js_sys::Array::new();
            for e in entries {
                let item = js_sys::Object::new();
                set(&item, "key", e.key.into());
                // Nie exposujemy wartosci jesli is_secret — chroni logs/devtools.
                if e.is_secret {
                    set(&item, "value", "<redacted>".into());
                } else {
                    set(&item, "value", e.value.into());
                }
                set(&item, "isSecret", e.is_secret.into());
                arr.push(&item.into());
            }
            set(&obj, "entries", arr.into());
        }
        MessageBody::SettingsUpdateRequestBody(req) => {
            set(&obj, "variant", "SettingsUpdateRequest".into());
            set(&obj, "entriesCount", (req.entries.len() as u32).into());
        }
        MessageBody::SettingsUpdateResponse { applied } => {
            set(&obj, "variant", "SettingsUpdateResponse".into());
            set(&obj, "applied", applied.into());
        }
        MessageBody::DashboardMetricsRequest => {
            set(&obj, "variant", "DashboardMetricsRequest".into());
        }
        MessageBody::DashboardMetricsResponse(s) => {
            set(&obj, "variant", "DashboardMetricsResponse".into());
            set(&obj, "cpuUsagePercent", (s.cpu_usage_percent as f64).into());
            set(&obj, "ramUsedMb", s.ram_used_mb.into());
            set(&obj, "ramTotalMb", s.ram_total_mb.into());
            set(&obj, "activeRequests", s.active_requests.into());
            set(&obj, "totalRequests", s.total_requests.into());
            set(&obj, "totalErrors", s.total_errors.into());
            set(&obj, "tokensPerSecond", s.tokens_per_second.into());
            set(&obj, "activeServices", (s.active_services as u32).into());
        }
        MessageBody::Error(err) => {
            set(&obj, "variant", "Error".into());
            set(&obj, "code", protocol_error_code_name(err.code).into());
            set(&obj, "message", err.message.into());
            if let Some(trace) = err.trace_id {
                set(&obj, "traceId", trace.into());
            }
        }
    }
    Ok(obj.into())
}

fn protocol_error_code_name(code: ProtocolErrorCode) -> &'static str {
    match code {
        ProtocolErrorCode::InvalidFrame => "InvalidFrame",
        ProtocolErrorCode::PolicyDenied => "PolicyDenied",
        ProtocolErrorCode::AuthRequired => "AuthRequired",
        ProtocolErrorCode::NodeUnreachable => "NodeUnreachable",
        ProtocolErrorCode::StreamCancelled => "StreamCancelled",
        ProtocolErrorCode::RateLimited => "RateLimited",
        ProtocolErrorCode::NotImplemented => "NotImplemented",
        ProtocolErrorCode::Internal => "Internal",
        ProtocolErrorCode::NotFound => "NotFound",
        ProtocolErrorCode::BadRequest => "BadRequest",
    }
}

// Suppress unused import warning for a helper never used in lib (reserved for internal use)
#[allow(dead_code)]
fn _keep_protocol_error_referenced(e: ProtocolError) -> ProtocolError {
    e
}

// =============================================================================
// Testy native (cargo test)
// =============================================================================

// =============================================================================
// Testy native — wolaja pure-Rust inner functions (bez wasm-bindgen JS shimow).
// Testy WASM-specyficzne (wasm-bindgen-test) doda sie pozniej gdy w CI bedziemy
// mieli wasm-pack test runner.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_schema_version_matches() {
        assert_eq!(PROTOCOL_SCHEMA_VERSION, 2);
    }

    #[test]
    fn roundtrip_envelope_with_node_list_request() {
        let body = encode_body_inner(&MessageBody::NodeListRequest).unwrap();
        let frame =
            encode_envelope_direct_inner(42, 1, message_kind::META_HEARTBEAT, body.clone())
                .unwrap();
        let env = rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&frame).unwrap();
        assert_eq!(env.correlation_id, 42);
        assert_eq!(env.sequence, 1);
        assert!(matches!(env.routing, Routing::Direct));
        assert_eq!(env.body, body);
    }

    #[test]
    fn validate_frame_accepts_good_and_rejects_bad() {
        let body = encode_body_inner(&MessageBody::NodeListRequest).unwrap();
        let frame = encode_envelope_direct_inner(1, 1, 0xF001, body).unwrap();
        assert!(rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&frame).is_ok());
        assert!(rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&[]).is_err());
        assert!(rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&[0u8; 8]).is_err());
        assert!(
            rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&frame[..frame.len() / 2]).is_err()
        );
    }

    #[test]
    fn node_info_request_requires_32_bytes() {
        assert!(encode_node_info_request_inner(&[0u8; 32]).is_ok());
        assert!(encode_node_info_request_inner(&[0u8; 10]).is_err());
        assert!(encode_node_info_request_inner(&[0u8; 64]).is_err());
    }

    #[test]
    fn body_encode_decode_round_trip_native() {
        let body = MessageBody::MetaHeartbeat {
            sent_at_epoch: 1_700_000_000,
        };
        let bytes = encode_body_inner(&body).unwrap();
        let decoded = rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn protocol_error_code_name_exhaustive() {
        for code in [
            ProtocolErrorCode::InvalidFrame,
            ProtocolErrorCode::PolicyDenied,
            ProtocolErrorCode::AuthRequired,
            ProtocolErrorCode::NodeUnreachable,
            ProtocolErrorCode::StreamCancelled,
            ProtocolErrorCode::RateLimited,
            ProtocolErrorCode::NotImplemented,
            ProtocolErrorCode::Internal,
            ProtocolErrorCode::NotFound,
            ProtocolErrorCode::BadRequest,
        ] {
            let name = protocol_error_code_name(code);
            assert!(!name.is_empty());
        }
    }
}
