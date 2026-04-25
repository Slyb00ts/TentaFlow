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
//     encodeModelListRequest, encodeMetaHeartbeat, decodeMessageBody,
//   } from './codec.js';
//   await init();
//   const body = encodeModelListRequest();
//   const frame = encodeEnvelopeDirect(1n, 1, messageKind.META_HEARTBEAT, body);
//   ws.send(frame);
// =============================================================================

use tentaflow_protocol::{
    envelope::{message_kind, Envelope, EnvelopeFlags, Routing},
    message_body::{
        AddonAdminOnlySetRequest, AddonConfigGetRequest, AddonConfigSetRequest, AddonDetailRequest,
        AddonShowInCatalogSetRequest,
        AddonInstallRequest, AddonLogsRequest, AddonNetworkRulesGetRequest,
        AddonNetworkRulesSetRequest, AddonOAuthAuthorizeStartRequest,
        AddonOAuthConfigClearSecretRequest, AddonOAuthConfigListRequest,
        AddonOAuthConfigSetRequest, AddonOAuthLinkedAccountsRequest, AddonOAuthReauthorizeRequest,
        AddonOAuthRevokeRequest, AddonOAuthTestConnectionRequest,
        AddonPermissionCatalogRequest, AddonPermissionCheckRequest,
        AddonPermissionDefaultSetRequest, AddonPermissionMatrixRequest, AddonPermissionSetRequest,
        AddonReloadRequest, AddonResourcesGetRequest, AddonResourcesSetRequest, AddonToggleRequest,
        AddonToolsRequest, AddonUninstallRequest, AddonVisibilityListRequest,
        AddonVisibilitySetRequest, ApiKeyCreateRequest,
        AuthLoginRequest, ChatMessage, ChatStreamRequest, ClusterAddMemberRequest,
        ClusterCreateRequest, ClusterDeleteRequest, ClusterDetailRequest,
        ClusterProbeStreamRequest, ClusterRemoveMemberRequest, ClusterUpdateRequest,
        FlowCreateRequest, FlowUpdateRequest, FlowVersionGetRequest, FlowVersionListRequest,
        FlowVersionRestoreRequest, MeshConnectRequest, MeshNodeCommandRequest,
        MeshNodeNetworkConfigRequest, MeshPairInitRequest, MeshPairingConfirmRequest,
        MeshPairingRejectRequest, MeshPairingStartRequest, MeshTrustRetrustRequest,
        MeshTrustRevokeRequest, MessageBody, ModelAliasCreateRequest, ModelAliasDeleteRequest,
        ModelAliasUpdateRequest, ModelInstallRequest, MyOAuthAccountsListRequest,
        NoteCreateRequest, NoteDeleteRequest, NoteDetailRequest, NoteSetPinnedRequest,
        NoteUpdateRequest, NotesListRequest, NotesRequest, NotesResponse, ProtocolError,
        ProtocolErrorCode, ServiceCreateRequest, ServiceDeployRequest,
        ServiceManifestDeployRequest, ServiceUpdateRequest, SettingEntry, SettingsUpdateRequest,
        SsoProviderCreateRequest, SsoProviderDeleteRequest, TranslateRequest, TtsRule,
    },
    SCHEMA_VERSION as PROTOCOL_SCHEMA_VERSION,
};
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
    sequence: u64,
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

/// MessageBody::TranslateRequest — synchroniczne tlumaczenie przez LLM.
/// `source_lang` = "auto" dla auto-detekcji; `tone` opcjonalny
/// ("formal"/"casual"/"neutral").
#[wasm_bindgen(js_name = encodeTranslateRequest)]
pub fn encode_translate_request(
    source_text: String,
    source_lang: String,
    target_lang: String,
    tone: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::TranslateBody(
        tentaflow_protocol::TranslatePayload::Req(TranslateRequest {
            source_text,
            source_lang,
            target_lang,
            tone,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterUpdateRequest. Wszystkie pola opcjonalne — `None`
/// zachowuje obecna wartosc na serwerze.
#[wasm_bindgen(js_name = encodeClusterUpdateRequest)]
pub fn encode_cluster_update_request(
    cluster_id: String,
    name: Option<String>,
    description: Option<String>,
    strategy: Option<String>,
    failover_enabled: Option<bool>,
    failover_target: Option<String>,
    health_check_interval_ms: Option<u32>,
    timeout_ms: Option<u32>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterUpdateRequestBody(ClusterUpdateRequest {
        cluster_id,
        name,
        description,
        strategy,
        failover_enabled,
        failover_target,
        health_check_interval_ms,
        timeout_ms,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterListRequest (unit variant).
#[wasm_bindgen(js_name = encodeClusterListRequest)]
pub fn encode_cluster_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterDetailRequest { cluster_id }.
#[wasm_bindgen(js_name = encodeClusterDetailRequest)]
pub fn encode_cluster_detail_request(cluster_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterDetailRequestBody(ClusterDetailRequest {
        cluster_id,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterCreateRequest.
#[wasm_bindgen(js_name = encodeClusterCreateRequest)]
pub fn encode_cluster_create_request(
    name: String,
    description: Option<String>,
    strategy: String,
    failover_enabled: bool,
    failover_target: Option<String>,
    health_check_interval_ms: u32,
    timeout_ms: u32,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterCreateRequestBody(ClusterCreateRequest {
        name,
        description,
        strategy,
        failover_enabled,
        failover_target,
        health_check_interval_ms,
        timeout_ms,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterDeleteRequest { cluster_id }.
#[wasm_bindgen(js_name = encodeClusterDeleteRequest)]
pub fn encode_cluster_delete_request(cluster_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterDeleteRequestBody(ClusterDeleteRequest {
        cluster_id,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterAddMemberRequest.
#[wasm_bindgen(js_name = encodeClusterAddMemberRequest)]
pub fn encode_cluster_add_member_request(
    cluster_id: String,
    node_id: String,
    interface_type: Option<String>,
    interface_speed_mbps: Option<u32>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterAddMemberRequestBody(ClusterAddMemberRequest {
        cluster_id,
        node_id,
        interface_type,
        interface_speed_mbps,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterRemoveMemberRequest.
#[wasm_bindgen(js_name = encodeClusterRemoveMemberRequest)]
pub fn encode_cluster_remove_member_request(
    cluster_id: String,
    node_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterRemoveMemberRequestBody(
        ClusterRemoveMemberRequest { cluster_id, node_id },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterProbeStreamRequest { node_ids }.
#[wasm_bindgen(js_name = encodeClusterProbeStreamRequest)]
pub fn encode_cluster_probe_stream_request(node_ids: Vec<String>) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterProbeStreamRequestBody(
        ClusterProbeStreamRequest { node_ids },
    ))
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

// ---- Mesh read-only views (FAZA 1a) ----

#[wasm_bindgen(js_name = encodeMeshNodeListRequest)]
pub fn encode_mesh_node_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshNodeListRequest).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeshNodeDetailRequest)]
pub fn encode_mesh_node_detail_request(node_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshNodeDetailRequestBody(
        tentaflow_protocol::MeshNodeDetailRequest { node_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeshPendingListRequest)]
pub fn encode_mesh_pending_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshPendingListRequest).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeshIdentityRequest)]
pub fn encode_mesh_identity_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshIdentityRequest).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeshServicesListRequest)]
pub fn encode_mesh_services_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshServicesListRequest).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeshTrustedListRequest)]
pub fn encode_mesh_trusted_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshTrustedListRequest).map_err(|e| JsError::new(&e))
}

// ---- Mesh write ops (FAZA 1b — pairing/trust/connect/command/network-config) ----

#[wasm_bindgen(js_name = encodeMeshPairingStartRequest)]
pub fn encode_mesh_pairing_start_request(
    remote_address: String,
    pin_hint: Option<String>,
    remote_public_key: Option<String>,
    remote_addresses: Option<Vec<String>>,
    remote_relay_url: Option<String>,
    remote_hostname: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshPairingStartRequestBody(
        MeshPairingStartRequest {
            remote_address,
            pin_hint: pin_hint.unwrap_or_default(),
            remote_public_key: remote_public_key.unwrap_or_default(),
            remote_addresses: remote_addresses.unwrap_or_default(),
            remote_relay_url: remote_relay_url.unwrap_or_default(),
            remote_hostname: remote_hostname.unwrap_or_default(),
        },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeshPairingConfirmRequest)]
pub fn encode_mesh_pairing_confirm_request(
    pair_id: String,
    pin: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshPairingConfirmRequestBody(
        MeshPairingConfirmRequest { pair_id, pin },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeshPairingRejectRequest)]
pub fn encode_mesh_pairing_reject_request(pair_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshPairingRejectRequestBody(
        MeshPairingRejectRequest { pair_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeshTrustRevokeRequest)]
pub fn encode_mesh_trust_revoke_request(node_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshTrustRevokeRequestBody(
        MeshTrustRevokeRequest { node_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeshTrustRetrustRequest)]
pub fn encode_mesh_trust_retrust_request(node_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshTrustRetrustRequestBody(
        MeshTrustRetrustRequest { node_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeshConnectRequest)]
pub fn encode_mesh_connect_request(address: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshConnectRequestBody(MeshConnectRequest {
        address,
    }))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeshNodeCommandRequest)]
pub fn encode_mesh_node_command_request(
    node_id: String,
    command: String,
    args: Vec<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshNodeCommandRequestBody(
        MeshNodeCommandRequest {
            node_id,
            command,
            args,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeshNodeNetworkConfigRequest)]
pub fn encode_mesh_node_network_config_request(
    node_id: String,
    interface_name: String,
    config_json: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeshNodeNetworkConfigRequestBody(
        MeshNodeNetworkConfigRequest {
            node_id,
            interface_name,
            config_json,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

// ---- Models unified + aliasy (FAZA 2) ----

#[wasm_bindgen(js_name = encodeModelsUnifiedListRequest)]
pub fn encode_models_unified_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelsUnifiedListRequest).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeModelAliasListRequest)]
pub fn encode_model_alias_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelAliasListRequest).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeModelAliasCreateRequest)]
pub fn encode_model_alias_create_request(
    alias: String,
    target_model: String,
    strategy: Option<String>,
    fallback_targets: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelAliasCreateRequestBody(
        ModelAliasCreateRequest {
            alias,
            target_model,
            strategy,
            fallback_targets,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeModelAliasUpdateRequest)]
pub fn encode_model_alias_update_request(
    id: f64,
    alias: String,
    target_model: String,
    is_active: Option<bool>,
    strategy: Option<String>,
    fallback_targets: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelAliasUpdateRequestBody(
        ModelAliasUpdateRequest {
            id: id as i64,
            alias,
            target_model,
            is_active,
            strategy,
            fallback_targets,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeModelAliasDeleteRequest)]
pub fn encode_model_alias_delete_request(id: f64) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelAliasDeleteRequestBody(
        ModelAliasDeleteRequest { id: id as i64 },
    ))
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

// ---- SSO / TLS / NGC (FAZA 4) ----

/// MessageBody::SsoProvidersListRequest (unit variant).
#[wasm_bindgen(js_name = encodeSsoProvidersListRequest)]
pub fn encode_sso_providers_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SsoProvidersListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::SsoProviderCreateRequest — pelne dane providera SSO/OIDC.
#[wasm_bindgen(js_name = encodeSsoProviderCreateRequest)]
pub fn encode_sso_provider_create_request(
    name: String,
    provider_type: String,
    client_id: String,
    client_secret: String,
    discovery_url: String,
    auto_create_users: bool,
    default_group_id: Option<f64>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SsoProviderCreateRequestBody(
        SsoProviderCreateRequest {
            name,
            provider_type,
            client_id,
            client_secret,
            discovery_url,
            auto_create_users,
            default_group_id: default_group_id.map(|v| v as i64),
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::SsoProviderDeleteRequest { id }.
#[wasm_bindgen(js_name = encodeSsoProviderDeleteRequest)]
pub fn encode_sso_provider_delete_request(id: f64) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SsoProviderDeleteRequestBody(
        SsoProviderDeleteRequest { id: id as i64 },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::TlsStatusRequest (unit variant).
#[wasm_bindgen(js_name = encodeTlsStatusRequest)]
pub fn encode_tls_status_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::TlsStatusRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::NgcStatusRequest (unit variant).
#[wasm_bindgen(js_name = encodeNgcStatusRequest)]
pub fn encode_ngc_status_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::NgcStatusRequest).map_err(|e| JsError::new(&e))
}

// ---- Catalog: NIM + manifest deploy (FAZA 5) ----

/// MessageBody::NimCatalogListRequest (unit variant).
#[wasm_bindgen(js_name = encodeNimCatalogListRequest)]
pub fn encode_nim_catalog_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::NimCatalogListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::DeploymentBody(ReqStart) — inicjuje deploy silnika z manifestu.
/// `config_json` przyjmujemy jako stringify JSON z GUI (elastyczna struktura).
/// Nazwa wasm-bindgen `encodeServiceManifestDeployRequest` zachowana dla
/// kompatybilności z frontend codec.js — pod spodem opakowujemy w
/// DeploymentBody::ReqStart (po konsolidacji na inner enum).
#[wasm_bindgen(js_name = encodeServiceManifestDeployRequest)]
pub fn encode_service_manifest_deploy_request(
    engine_id: String,
    deploy_method: String,
    node_id: String,
    config_json: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::DeploymentBody(
        tentaflow_protocol::DeploymentPayload::ReqStart(ServiceManifestDeployRequest {
            engine_id,
            deploy_method,
            node_id,
            config_json,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeDeploymentStatusRequest)]
pub fn encode_deployment_status_request(deploy_id: String) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{DeploymentPayload, DeploymentStatusRequest};
    encode_body_inner(&MessageBody::DeploymentBody(DeploymentPayload::ReqStatus(
        DeploymentStatusRequest { deploy_id },
    )))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeDeploymentListRequest)]
pub fn encode_deployment_list_request(
    engine_id: String,
    status: String,
    only_mine: bool,
    limit: i32,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{DeploymentListRequest, DeploymentPayload};
    encode_body_inner(&MessageBody::DeploymentBody(DeploymentPayload::ReqList(
        DeploymentListRequest {
            engine_id,
            status,
            only_mine,
            limit,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeDeploymentLogStreamRequest)]
pub fn encode_deployment_log_stream_request(
    deploy_id: String,
    replay_tail: bool,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{DeploymentLogStreamRequest, DeploymentPayload};
    encode_body_inner(&MessageBody::DeploymentBody(DeploymentPayload::ReqLogStream(
        DeploymentLogStreamRequest {
            deploy_id,
            replay_tail,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::DeploymentBody(DeploymentPayload::ReqRedeploy). Force flag asks
/// backend to terminate active sessions (agents) rather than returning
/// `active_sessions`.
#[wasm_bindgen(js_name = encodeServiceRedeployRequest)]
pub fn encode_service_redeploy_request(
    service_id: f64,
    force_if_active_sessions: bool,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{DeploymentPayload, ServiceRedeployRequest};
    encode_body_inner(&MessageBody::DeploymentBody(DeploymentPayload::ReqRedeploy(
        ServiceRedeployRequest {
            service_id: service_id as i64,
            force_if_active_sessions,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

// ---- Meeting VNC tunnel (same-node websockify bridge) ----

/// MessageBody::VncTunnelBody(ReqOpen) — start streaming tunnel for session.
#[wasm_bindgen(js_name = encodeVncTunnelOpenRequest)]
pub fn encode_vnc_tunnel_open_request(session_id: f64) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{VncTunnelOpenRequest, VncTunnelPayload};
    encode_body_inner(&MessageBody::VncTunnelBody(VncTunnelPayload::ReqOpen(
        VncTunnelOpenRequest {
            session_id: session_id as i64,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::VncTunnelBody(ReqSend) — browser → container RFB bytes.
#[wasm_bindgen(js_name = encodeVncTunnelSendRequest)]
pub fn encode_vnc_tunnel_send_request(
    tunnel_id: String,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{VncTunnelPayload, VncTunnelSendRequest};
    encode_body_inner(&MessageBody::VncTunnelBody(VncTunnelPayload::ReqSend(
        VncTunnelSendRequest { tunnel_id, bytes },
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::VncTunnelBody(ReqClose) — tear down tunnel explicitly.
#[wasm_bindgen(js_name = encodeVncTunnelCloseRequest)]
pub fn encode_vnc_tunnel_close_request(tunnel_id: String) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{VncTunnelCloseRequest, VncTunnelPayload};
    encode_body_inner(&MessageBody::VncTunnelBody(VncTunnelPayload::ReqClose(
        VncTunnelCloseRequest { tunnel_id },
    )))
    .map_err(|e| JsError::new(&e))
}

// ---- Meeting browser capture (screenshot / DOM snapshot) ----

/// MessageBody::BrowserCaptureRequest — one-shot capture of the bot's page.
#[wasm_bindgen(js_name = encodeBrowserCaptureRequest)]
pub fn encode_browser_capture_request(
    session_id: f64,
    kind: String,
    full_page: bool,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{BrowserCapturePayload, BrowserCaptureRequest};
    encode_body_inner(&MessageBody::BrowserCaptureBody(
        BrowserCapturePayload::Request(BrowserCaptureRequest {
            session_id: session_id as i64,
            kind,
            full_page,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

// ---- Addons + Users (FAZA 6) ----

/// MessageBody::AddonsListRequest (unit variant).
#[wasm_bindgen(js_name = encodeAddonsListRequest)]
pub fn encode_addons_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonsListRequest).map_err(|e| JsError::new(&e))
}

/// LEGACY UsersListRequest — zastapione przez encodeIamListUsersRequest.
#[wasm_bindgen(js_name = encodeUsersListRequest)]
pub fn encode_users_list_request() -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqListUsers)
}

// =============================================================================
// Addon permissions + OAuth (migracja 38) — encodery request variantow
// =============================================================================

/// MessageBody::AddonDetailRequest { addon_id } — szczegoly addona.
#[wasm_bindgen(js_name = encodeAddonDetailRequest)]
pub fn encode_addon_detail_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonDetailRequestBody(AddonDetailRequest {
        addon_id,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonVisibilityListRequest { addon_id } — widocznosc per grupa.
#[wasm_bindgen(js_name = encodeAddonVisibilityListRequest)]
pub fn encode_addon_visibility_list_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonVisibilityListRequestBody(
        AddonVisibilityListRequest { addon_id },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonVisibilitySetRequest { addon_id, group_id, visible }.
#[wasm_bindgen(js_name = encodeAddonVisibilitySetRequest)]
pub fn encode_addon_visibility_set_request(
    addon_id: String,
    group_id: f64,
    visible: bool,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonVisibilitySetRequestBody(
        AddonVisibilitySetRequest {
            addon_id,
            group_id: group_id as i64,
            visible,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonAdminOnlySetRequest { addon_id, admin_only }.
#[wasm_bindgen(js_name = encodeAddonAdminOnlySetRequest)]
pub fn encode_addon_admin_only_set_request(
    addon_id: String,
    admin_only: bool,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonAdminOnlySetRequestBody(
        AddonAdminOnlySetRequest {
            addon_id,
            admin_only,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonShowInCatalogSetRequest { addon_id, show_in_catalog }.
#[wasm_bindgen(js_name = encodeAddonShowInCatalogSetRequest)]
pub fn encode_addon_show_in_catalog_set_request(
    addon_id: String,
    show_in_catalog: bool,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonShowInCatalogSetRequestBody(
        AddonShowInCatalogSetRequest {
            addon_id,
            show_in_catalog,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonPermissionCatalogRequest { addon_id } — katalog deklaracji.
#[wasm_bindgen(js_name = encodeAddonPermissionCatalogRequest)]
pub fn encode_addon_permission_catalog_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonPermissionCatalogRequestBody(
        AddonPermissionCatalogRequest { addon_id },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonPermissionMatrixRequest { addon_id } — aktualna macierz.
#[wasm_bindgen(js_name = encodeAddonPermissionMatrixRequest)]
pub fn encode_addon_permission_matrix_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonPermissionMatrixRequestBody(
        AddonPermissionMatrixRequest { addon_id },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonPermissionSetRequest — ustawia grant per (user|group).
#[wasm_bindgen(js_name = encodeAddonPermissionSetRequest)]
pub fn encode_addon_permission_set_request(
    addon_id: String,
    subject_type: String,
    subject_id: f64,
    permission_id: String,
    grant_mode: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonPermissionSetRequestBody(
        AddonPermissionSetRequest {
            addon_id,
            subject_type,
            subject_id: subject_id as i64,
            permission_id,
            grant_mode,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonPermissionDefaultSetRequest — ustawia domyslny grant addona.
#[wasm_bindgen(js_name = encodeAddonPermissionDefaultSetRequest)]
pub fn encode_addon_permission_default_set_request(
    addon_id: String,
    permission_id: String,
    grant_mode: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonPermissionDefaultSetRequestBody(
        AddonPermissionDefaultSetRequest {
            addon_id,
            permission_id,
            grant_mode,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonPermissionCheckRequest — czy uzytkownik ma uprawnienie.
/// `user_id` = None (pass null z JS) => serwer uzyje id z sesji.
#[wasm_bindgen(js_name = encodeAddonPermissionCheckRequest)]
pub fn encode_addon_permission_check_request(
    addon_id: String,
    permission_id: String,
    user_id: Option<f64>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonPermissionCheckRequestBody(
        AddonPermissionCheckRequest {
            addon_id,
            permission_id,
            user_id: user_id.map(|v| v as i64),
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonOAuthConfigListRequest { addon_id } — zero secretow.
#[wasm_bindgen(js_name = encodeAddonOAuthConfigListRequest)]
pub fn encode_addon_oauth_config_list_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonOAuthConfigListRequestBody(
        AddonOAuthConfigListRequest { addon_id },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonOAuthConfigSetRequest — zapis konfiguracji OAuth.
/// `client_secret` = None (null) => zachowaj obecny, Some(..) => nadpisz.
#[wasm_bindgen(js_name = encodeAddonOAuthConfigSetRequest)]
pub fn encode_addon_oauth_config_set_request(
    addon_id: String,
    provider_id: String,
    client_id: String,
    client_secret: Option<String>,
    redirect_uri: String,
    enabled: bool,
    oauth_mode: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonOAuthConfigSetRequestBody(
        AddonOAuthConfigSetRequest {
            addon_id,
            provider_id,
            client_id,
            client_secret,
            redirect_uri,
            enabled,
            oauth_mode,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonOAuthConfigClearSecretRequest — usun wylacznie secret.
#[wasm_bindgen(js_name = encodeAddonOAuthConfigClearSecretRequest)]
pub fn encode_addon_oauth_config_clear_secret_request(
    addon_id: String,
    provider_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonOAuthConfigClearSecretRequestBody(
        AddonOAuthConfigClearSecretRequest {
            addon_id,
            provider_id,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonOAuthAuthorizeStartRequest — inicjuje flow autoryzacji.
#[wasm_bindgen(js_name = encodeAddonOAuthAuthorizeStartRequest)]
pub fn encode_addon_oauth_authorize_start_request(
    addon_id: String,
    provider_id: String,
    mode: String,
    redirect_after: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonOAuthAuthorizeStartRequestBody(
        AddonOAuthAuthorizeStartRequest {
            addon_id,
            provider_id,
            mode,
            redirect_after,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonOAuthLinkedAccountsRequest — lista polaczonych kont.
/// `scope` = "all" (admin) lub "mine" (user).
#[wasm_bindgen(js_name = encodeAddonOAuthLinkedAccountsRequest)]
pub fn encode_addon_oauth_linked_accounts_request(
    addon_id: String,
    scope: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonOAuthLinkedAccountsRequestBody(
        AddonOAuthLinkedAccountsRequest { addon_id, scope },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonOAuthRevokeRequest { account_id }.
#[wasm_bindgen(js_name = encodeAddonOAuthRevokeRequest)]
pub fn encode_addon_oauth_revoke_request(account_id: f64) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonOAuthRevokeRequestBody(
        AddonOAuthRevokeRequest {
            account_id: account_id as i64,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonOAuthReauthorizeRequest { account_id }.
#[wasm_bindgen(js_name = encodeAddonOAuthReauthorizeRequest)]
pub fn encode_addon_oauth_reauthorize_request(account_id: f64) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonOAuthReauthorizeRequestBody(
        AddonOAuthReauthorizeRequest {
            account_id: account_id as i64,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonOAuthTestConnectionRequest { addon_id, provider_id }.
#[wasm_bindgen(js_name = encodeAddonOAuthTestConnectionRequest)]
pub fn encode_addon_oauth_test_connection_request(
    addon_id: String,
    provider_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonOAuthTestConnectionRequestBody(
        AddonOAuthTestConnectionRequest {
            addon_id,
            provider_id,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::MyOAuthAccountsListRequest (unit) — lista kont biezacego usera.
#[wasm_bindgen(js_name = encodeMyOAuthAccountsListRequest)]
pub fn encode_my_oauth_accounts_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MyOAuthAccountsListRequestBody(
        MyOAuthAccountsListRequest,
    ))
    .map_err(|e| JsError::new(&e))
}

// ---- Audit log screen (Admin only) -------------------------------------

/// Buduje `AuditLogFilters` z pol nullable — wszystkie parametry optional.
fn build_audit_filters(
    user_id: Option<f64>,
    addon_id: Option<String>,
    action: Option<String>,
    from_date: Option<String>,
    to_date: Option<String>,
    search: Option<String>,
) -> tentaflow_protocol::AuditLogFilters {
    tentaflow_protocol::AuditLogFilters {
        user_id: user_id.map(|v| v as i64),
        addon_id,
        action,
        from_date,
        to_date,
        search,
    }
}

/// MessageBody::AuditLogListRequest — lista logu z filtrami + paginacja.
#[wasm_bindgen(js_name = encodeAuditLogListRequest)]
pub fn encode_audit_log_list_request(
    user_id: Option<f64>,
    addon_id: Option<String>,
    action: Option<String>,
    from_date: Option<String>,
    to_date: Option<String>,
    search: Option<String>,
    offset: f64,
    limit: u32,
) -> Result<Vec<u8>, JsError> {
    let filters = build_audit_filters(user_id, addon_id, action, from_date, to_date, search);
    encode_body_inner(&MessageBody::AuditLogListRequestBody(
        tentaflow_protocol::AuditLogListRequest {
            filters,
            offset: offset.max(0.0) as u64,
            limit: limit.min(1000),
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AuditLogExportRequest — eksport CSV z filtrami.
#[wasm_bindgen(js_name = encodeAuditLogExportRequest)]
pub fn encode_audit_log_export_request(
    user_id: Option<f64>,
    addon_id: Option<String>,
    action: Option<String>,
    from_date: Option<String>,
    to_date: Option<String>,
    search: Option<String>,
) -> Result<Vec<u8>, JsError> {
    let filters = build_audit_filters(user_id, addon_id, action, from_date, to_date, search);
    encode_body_inner(&MessageBody::AuditLogExportRequestBody(
        tentaflow_protocol::AuditLogExportRequest { filters },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AuditLogCleanupRequest — usun wpisy starsze niz N dni.
#[wasm_bindgen(js_name = encodeAuditLogCleanupRequest)]
pub fn encode_audit_log_cleanup_request(keep_days: u32) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AuditLogCleanupRequestBody(
        tentaflow_protocol::AuditLogCleanupRequest { keep_days },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::SubscribeResumeRequest { resume_token }.
/// Klient po reconnect przekazuje token z poprzedniej SubscribeResumeOffer.
#[wasm_bindgen(js_name = encodeSubscribeResumeRequest)]
pub fn encode_subscribe_resume_request(resume_token: Vec<u8>) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SubscribeResumeRequest { resume_token })
        .map_err(|e| JsError::new(&e))
}

// --- Models ---------------------------------------------------------------

/// MessageBody::ModelDetailRequest { model_id }.
#[wasm_bindgen(js_name = encodeModelDetailRequest)]
pub fn encode_model_detail_request(model_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelDetailRequest { model_id }).map_err(|e| JsError::new(&e))
}

/// MessageBody::ModelInstallRequest { model_id, source_repo }.
#[wasm_bindgen(js_name = encodeModelInstallRequest)]
pub fn encode_model_install_request(
    model_id: String,
    source_repo: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelInstallRequestBody(ModelInstallRequest {
        model_id,
        source_repo,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ModelDeleteRequest { model_id }.
#[wasm_bindgen(js_name = encodeModelDeleteRequest)]
pub fn encode_model_delete_request(model_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelDeleteRequest { model_id }).map_err(|e| JsError::new(&e))
}

// --- Hub ------------------------------------------------------------------

/// MessageBody::HubEngineListRequest (unit).
#[wasm_bindgen(js_name = encodeHubEngineListRequest)]
pub fn encode_hub_engine_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::HubEngineListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::HubModelSearchRequest { query }.
#[wasm_bindgen(js_name = encodeHubModelSearchRequest)]
pub fn encode_hub_model_search_request(query: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::HubModelSearchRequest { query }).map_err(|e| JsError::new(&e))
}

// --- Flows ----------------------------------------------------------------

/// MessageBody::FlowListRequest (unit).
#[wasm_bindgen(js_name = encodeFlowListRequest)]
pub fn encode_flow_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FlowListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::FlowDetailRequest { flow_id }.
#[wasm_bindgen(js_name = encodeFlowDetailRequest)]
pub fn encode_flow_detail_request(flow_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FlowDetailRequest { flow_id }).map_err(|e| JsError::new(&e))
}

/// MessageBody::FlowCreateRequest { name, description, graph_json }.
#[wasm_bindgen(js_name = encodeFlowCreateRequest)]
pub fn encode_flow_create_request(
    name: String,
    description: Option<String>,
    graph_json: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FlowCreateRequestBody(FlowCreateRequest {
        name,
        description,
        graph_json,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::FlowDeleteRequest { flow_id }.
#[wasm_bindgen(js_name = encodeFlowDeleteRequest)]
pub fn encode_flow_delete_request(flow_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FlowDeleteRequest { flow_id }).map_err(|e| JsError::new(&e))
}

/// MessageBody::FlowExecutionsListRequest { flow_id }.
#[wasm_bindgen(js_name = encodeFlowExecutionsListRequest)]
pub fn encode_flow_executions_list_request(flow_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FlowExecutionsListRequest { flow_id })
        .map_err(|e| JsError::new(&e))
}

/// MessageBody::FlowUpdateRequest — partial update flow.
#[wasm_bindgen(js_name = encodeFlowUpdateRequest)]
pub fn encode_flow_update_request(
    flow_id: String,
    name: Option<String>,
    description: Option<String>,
    flow_json: Option<String>,
    status: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FlowUpdateRequestBody(FlowUpdateRequest {
        flow_id,
        name,
        description,
        flow_json,
        status,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::FlowNodeTemplatesListRequest (unit).
#[wasm_bindgen(js_name = encodeFlowNodeTemplatesListRequest)]
pub fn encode_flow_node_templates_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FlowNodeTemplatesListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::FlowVersionListRequest { flow_id }.
#[wasm_bindgen(js_name = encodeFlowVersionListRequest)]
pub fn encode_flow_version_list_request(flow_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FlowVersionListRequestBody(FlowVersionListRequest {
        flow_id,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::FlowVersionGetRequest { flow_id, version_id }.
#[wasm_bindgen(js_name = encodeFlowVersionGetRequest)]
pub fn encode_flow_version_get_request(
    flow_id: String,
    version_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FlowVersionGetRequestBody(FlowVersionGetRequest {
        flow_id,
        version_id,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::FlowVersionRestoreRequest { flow_id, version_id }.
#[wasm_bindgen(js_name = encodeFlowVersionRestoreRequest)]
pub fn encode_flow_version_restore_request(
    flow_id: String,
    version_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FlowVersionRestoreRequestBody(
        FlowVersionRestoreRequest {
            flow_id,
            version_id,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

// --- Services -------------------------------------------------------------

/// MessageBody::ServiceListRequest (unit).
#[wasm_bindgen(js_name = encodeServiceListRequest)]
pub fn encode_service_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ServiceListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceStopRequest { service_id }.
#[wasm_bindgen(js_name = encodeServiceStopRequest)]
pub fn encode_service_stop_request(service_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ServiceStopRequest { service_id })
        .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceFlagsUpdateRequest { service_id, pinned, paused }.
/// pinned/paused jako i32 (-1 = nie zmieniaj, 0 = false, 1 = true).
#[wasm_bindgen(js_name = encodeServiceFlagsUpdateRequest)]
pub fn encode_service_flags_update_request(
    service_id: String,
    pinned: i32,
    paused: i32,
) -> Result<Vec<u8>, JsError> {
    let pinned_opt = if pinned < 0 { None } else { Some(pinned != 0) };
    let paused_opt = if paused < 0 { None } else { Some(paused != 0) };
    encode_body_inner(&MessageBody::ServiceFlagsBody(
        tentaflow_protocol::ServiceFlagsPayload::Req(
            tentaflow_protocol::ServiceFlagsUpdateRequest {
                service_id,
                pinned: pinned_opt,
                paused: paused_opt,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceDeployRequest { engine_id, model_id, deploy_method, node_id }.
/// node_id MUSI byc 32 bajtami.
#[wasm_bindgen(js_name = encodeServiceDeployRequest)]
pub fn encode_service_deploy_request(
    engine_id: String,
    model_id: String,
    deploy_method: String,
    node_id: &[u8],
) -> Result<Vec<u8>, JsError> {
    if node_id.len() != 32 {
        return Err(JsError::new("node_id must be exactly 32 bytes"));
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(node_id);
    encode_body_inner(&MessageBody::ServiceDeployRequestBody(ServiceDeployRequest {
        engine_id,
        model_id,
        deploy_method,
        node_id: buf,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceCreateRequest { name, service_type, strategy, config_json,
/// node_id?, cluster_id? }. `node_id` jest hex-enkodowanym ciagiem 64 znakow
/// (32 bajty), pusty string traktowany jako None.
#[wasm_bindgen(js_name = encodeServiceCreateRequest)]
pub fn encode_service_create_request(
    name: String,
    service_type: String,
    strategy: String,
    config_json: String,
    node_id: Option<String>,
    cluster_id: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ServiceCreateRequestBody(ServiceCreateRequest {
        name,
        service_type,
        strategy,
        config_json,
        node_id: node_id.filter(|s| !s.is_empty()),
        cluster_id: cluster_id.filter(|s| !s.is_empty()),
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceUpdateRequest { id, name, service_type, strategy, status,
/// config_json, node_id?, cluster_id? }.
#[wasm_bindgen(js_name = encodeServiceUpdateRequest)]
pub fn encode_service_update_request(
    id: String,
    name: String,
    service_type: String,
    strategy: String,
    status: String,
    config_json: String,
    node_id: Option<String>,
    cluster_id: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ServiceUpdateRequestBody(ServiceUpdateRequest {
        id,
        name,
        service_type,
        strategy,
        status,
        config_json,
        node_id: node_id.filter(|s| !s.is_empty()),
        cluster_id: cluster_id.filter(|s| !s.is_empty()),
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceQuicStatusRequest (unit). Periodyczne polling QUIC.
#[wasm_bindgen(js_name = encodeServiceQuicStatusRequest)]
pub fn encode_service_quic_status_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ServiceQuicStatusRequest).map_err(|e| JsError::new(&e))
}

// --- Prompts --------------------------------------------------------------

/// MessageBody::PromptListRequest (unit).
#[wasm_bindgen(js_name = encodePromptListRequest)]
pub fn encode_prompt_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::PromptListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::PromptDetailRequest { prompt_id }.
#[wasm_bindgen(js_name = encodePromptDetailRequest)]
pub fn encode_prompt_detail_request(prompt_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::PromptDetailRequest { prompt_id })
        .map_err(|e| JsError::new(&e))
}

// --- Notes ----------------------------------------------------------------

/// NotesRequest::List — empty inner struct.
#[wasm_bindgen(js_name = encodeNotesListRequest)]
pub fn encode_notes_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::NotesRequestBody(NotesRequest::List(
        NotesListRequest {},
    )))
    .map_err(|e| JsError::new(&e))
}

/// NotesRequest::Detail { note_id }.
#[wasm_bindgen(js_name = encodeNoteDetailRequest)]
pub fn encode_note_detail_request(note_id: f64) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::NotesRequestBody(NotesRequest::Detail(
        NoteDetailRequest {
            note_id: note_id as i64,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

/// NotesRequest::Create { title, body }.
#[wasm_bindgen(js_name = encodeNoteCreateRequest)]
pub fn encode_note_create_request(title: String, body: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::NotesRequestBody(NotesRequest::Create(
        NoteCreateRequest { title, body },
    )))
    .map_err(|e| JsError::new(&e))
}

/// NotesRequest::Update { note_id, title, body }.
#[wasm_bindgen(js_name = encodeNoteUpdateRequest)]
pub fn encode_note_update_request(
    note_id: f64,
    title: String,
    body: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::NotesRequestBody(NotesRequest::Update(
        NoteUpdateRequest {
            note_id: note_id as i64,
            title,
            body,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

/// NotesRequest::SetPinned { note_id, pinned }.
#[wasm_bindgen(js_name = encodeNoteSetPinnedRequest)]
pub fn encode_note_set_pinned_request(note_id: f64, pinned: bool) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::NotesRequestBody(NotesRequest::SetPinned(
        NoteSetPinnedRequest {
            note_id: note_id as i64,
            pinned,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

/// NotesRequest::Delete { note_id }.
#[wasm_bindgen(js_name = encodeNoteDeleteRequest)]
pub fn encode_note_delete_request(note_id: f64) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::NotesRequestBody(NotesRequest::Delete(
        NoteDeleteRequest {
            note_id: note_id as i64,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

// --- Meeting Bot ----------------------------------------------------------

use tentaflow_protocol::{
    MeetingActionItemStatusUpdateRequest, MeetingActionItemsListRequest,
    MeetingActiveSessionRequest, MeetingPayload, MeetingSessionDetailRequest,
    MeetingSessionLeaveRequest, MeetingSessionListRequest, MeetingSessionStartRequest,
    MeetingSettingKv, MeetingSettingsGetRequest, MeetingSettingsUpdateRequest,
    MeetingSummariesListRequest, MeetingTranscriptExportRequest,
    MeetingTranscriptsListRequest,
};

#[wasm_bindgen(js_name = encodeMeetingSessionStartRequest)]
pub fn encode_meeting_session_start(
    meeting_url: String,
    title: String,
    platform: String,
    bot_name: String,
    stt_alias: String,
    tts_alias: String,
    llm_alias: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeetingBody(MeetingPayload::ReqSessionStart(
        MeetingSessionStartRequest {
            meeting_url,
            title,
            platform,
            bot_name,
            stt_alias,
            tts_alias,
            llm_alias,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeetingSessionLeaveRequest)]
pub fn encode_meeting_session_leave(session_id: f64) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeetingBody(MeetingPayload::ReqSessionLeave(
        MeetingSessionLeaveRequest {
            session_id: session_id as i64,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeetingSessionListRequest)]
pub fn encode_meeting_session_list(only_mine: bool) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeetingBody(MeetingPayload::ReqSessionList(
        MeetingSessionListRequest { only_mine },
    )))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeetingSessionDetailRequest)]
pub fn encode_meeting_session_detail(
    session_id: f64,
    include_transcripts: bool,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeetingBody(MeetingPayload::ReqSessionDetail(
        MeetingSessionDetailRequest {
            session_id: session_id as i64,
            include_transcripts,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeetingTranscriptsListRequest)]
pub fn encode_meeting_transcripts_list(
    session_id: f64,
    since_ms: f64,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeetingBody(MeetingPayload::ReqTranscriptsList(
        MeetingTranscriptsListRequest {
            session_id: session_id as i64,
            since_ms: since_ms as i64,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeetingActiveSessionRequest)]
pub fn encode_meeting_active_session() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeetingBody(MeetingPayload::ReqActiveSession(
        MeetingActiveSessionRequest {},
    )))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeetingSettingsGetRequest)]
pub fn encode_meeting_settings_get() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeetingBody(MeetingPayload::ReqSettingsGet(
        MeetingSettingsGetRequest {},
    )))
    .map_err(|e| JsError::new(&e))
}

/// `settings` jest JS Array<[key, value]>. Konwertujemy pary do Vec<MeetingSettingKv>.
#[wasm_bindgen(js_name = encodeMeetingSettingsUpdateRequest)]
pub fn encode_meeting_settings_update(settings: JsValue) -> Result<Vec<u8>, JsError> {
    let arr: js_sys::Array = settings
        .dyn_into()
        .map_err(|_| JsError::new("settings musi byc Array<[key, value]>"))?;
    let mut kvs: Vec<MeetingSettingKv> = Vec::new();
    for i in 0..arr.length() {
        let pair: js_sys::Array = arr
            .get(i)
            .dyn_into()
            .map_err(|_| JsError::new("element musi byc [key, value]"))?;
        let key = pair
            .get(0)
            .as_string()
            .ok_or_else(|| JsError::new("key musi byc string"))?;
        let value = pair
            .get(1)
            .as_string()
            .ok_or_else(|| JsError::new("value musi byc string"))?;
        kvs.push(MeetingSettingKv { key, value });
    }
    encode_body_inner(&MessageBody::MeetingBody(MeetingPayload::ReqSettingsUpdate(
        MeetingSettingsUpdateRequest { settings: kvs },
    )))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeetingSummariesListRequest)]
pub fn encode_meeting_summaries_list(
    meeting_key: String,
    limit: Option<u32>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeetingBody(MeetingPayload::ReqSummariesList(
        MeetingSummariesListRequest { meeting_key, limit },
    )))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeetingActionItemsListRequest)]
pub fn encode_meeting_action_items_list(
    meeting_key: String,
    status_filter: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeetingBody(MeetingPayload::ReqActionItemsList(
        MeetingActionItemsListRequest {
            meeting_key,
            status_filter,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeetingActionItemStatusUpdateRequest)]
pub fn encode_meeting_action_item_status_update(
    item_id: f64,
    status: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeetingBody(
        MeetingPayload::ReqActionItemStatusUpdate(MeetingActionItemStatusUpdateRequest {
            item_id: item_id as i64,
            status,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMeetingTranscriptExportRequest)]
pub fn encode_meeting_transcript_export(meeting_key: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeetingBody(MeetingPayload::ReqTranscriptExport(
        MeetingTranscriptExportRequest { meeting_key },
    )))
    .map_err(|e| JsError::new(&e))
}

// --- Registries -----------------------------------------------------------

/// MessageBody::RegistryListRequest (unit).
#[wasm_bindgen(js_name = encodeRegistryListRequest)]
pub fn encode_registry_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::RegistryListRequest).map_err(|e| JsError::new(&e))
}

// --- TTS rules ------------------------------------------------------------

/// MessageBody::TtsRuleListRequest (unit).
#[wasm_bindgen(js_name = encodeTtsRuleListRequest)]
pub fn encode_tts_rule_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::TtsRuleListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::TtsRuleCreateRequest(TtsRule).
#[wasm_bindgen(js_name = encodeTtsRuleCreateRequest)]
pub fn encode_tts_rule_create_request(
    id: String,
    pattern: String,
    voice_id: String,
    priority: i32,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::TtsRuleCreateRequest(TtsRule {
        id,
        pattern,
        voice_id,
        priority,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::TtsRuleDeleteRequest { rule_id }.
#[wasm_bindgen(js_name = encodeTtsRuleDeleteRequest)]
pub fn encode_tts_rule_delete_request(rule_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::TtsRuleDeleteRequest { rule_id })
        .map_err(|e| JsError::new(&e))
}

// --- PII rules ------------------------------------------------------------

/// MessageBody::PiiRuleListRequest (unit).
#[wasm_bindgen(js_name = encodePiiRuleListRequest)]
pub fn encode_pii_rule_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::PiiRuleListRequest).map_err(|e| JsError::new(&e))
}

// --- Fast-path ------------------------------------------------------------

/// MessageBody::FastPathListRequest (unit).
#[wasm_bindgen(js_name = encodeFastPathListRequest)]
pub fn encode_fast_path_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FastPathListRequest).map_err(|e| JsError::new(&e))
}

// --- Settings (multi-entry) -----------------------------------------------

/// MessageBody::SettingsUpdateRequest — trzy rownolegle tablice (keys/values/is_secrets).
/// Wszystkie 3 musza miec ten sam dlugosc. Pozwala na batch update z JS bez
/// serde-wasm-bindgen.
#[wasm_bindgen(js_name = encodeSettingsUpdateBatch)]
pub fn encode_settings_update_batch(
    keys: Vec<String>,
    values: Vec<String>,
    is_secrets: Vec<u8>,
) -> Result<Vec<u8>, JsError> {
    if keys.len() != values.len() || keys.len() != is_secrets.len() {
        return Err(JsError::new(
            "keys, values, is_secrets must have same length",
        ));
    }
    let entries = keys
        .into_iter()
        .zip(values.into_iter())
        .zip(is_secrets.into_iter())
        .map(|((key, value), secret)| SettingEntry {
            key,
            value,
            is_secret: secret != 0,
        })
        .collect();
    encode_body_inner(&MessageBody::SettingsUpdateRequestBody(SettingsUpdateRequest {
        entries,
    }))
    .map_err(|e| JsError::new(&e))
}

// =============================================================================
// MessageBody decode (zwraca JS object z variant tag + polami)
// =============================================================================

fn set(obj: &js_sys::Object, key: &str, value: JsValue) {
    let _ = js_sys::Reflect::set(obj, &key.into(), &value);
}

/// Dekoduje rkyv-zakodowany MessageBody na JS object.
/// Dla znanych variantow zwraca obiekt z polem `variant`, a dla nieznanego
/// variantu `{ variant: "Unknown" }`.
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
        MessageBody::ModelListRequest => {
            set(&obj, "variant", "ModelListRequest".into());
        }
        MessageBody::ModelListResponse { models } => {
            set(&obj, "variant", "ModelListResponse".into());
            let arr = js_sys::Array::new();
            for m in models {
                let item = js_sys::Object::new();
                set(&item, "id", m.id.into());
                set(&item, "displayName", m.display_name.clone().into());
                set(&item, "display_name", m.display_name.into());
                set(&item, "category", m.category.into());
                set(&item, "engineId", m.engine_id.clone().into());
                set(&item, "engine_id", m.engine_id.into());
                set(&item, "availability", m.availability.into());
                arr.push(&item.into());
            }
            set(&obj, "models", arr.into());
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
        MessageBody::TranslateBody(tentaflow_protocol::TranslatePayload::Req(req)) => {
            set(&obj, "variant", "TranslateRequest".into());
            set(&obj, "sourceText", req.source_text.into());
            set(&obj, "sourceLang", req.source_lang.into());
            set(&obj, "targetLang", req.target_lang.into());
            if let Some(tone) = req.tone { set(&obj, "tone", tone.into()); }
        }
        MessageBody::TranslateBody(tentaflow_protocol::TranslatePayload::Res(resp)) => {
            set(&obj, "variant", "TranslateResponse".into());
            set(&obj, "translatedText", resp.translated_text.into());
            if let Some(d) = resp.detected_source_lang { set(&obj, "detectedSourceLang", d.into()); }
            set(&obj, "modelUsed", resp.model_used.into());
            set(&obj, "tokensUsed", resp.tokens_used.into());
        }
        MessageBody::ClusterUpdateRequestBody(req) => {
            set(&obj, "variant", "ClusterUpdateRequest".into());
            set(&obj, "clusterId", req.cluster_id.into());
            if let Some(n) = req.name { set(&obj, "name", n.into()); }
            if let Some(d) = req.description { set(&obj, "description", d.into()); }
            if let Some(s) = req.strategy { set(&obj, "strategy", s.into()); }
            if let Some(b) = req.failover_enabled { set(&obj, "failoverEnabled", b.into()); }
            if let Some(t) = req.failover_target { set(&obj, "failoverTarget", t.into()); }
            if let Some(v) = req.health_check_interval_ms { set(&obj, "healthCheckIntervalMs", v.into()); }
            if let Some(v) = req.timeout_ms { set(&obj, "timeoutMs", v.into()); }
        }
        MessageBody::ClusterUpdateResponseBody(resp) => {
            set(&obj, "variant", "ClusterUpdateResponse".into());
            set(&obj, "ok", resp.ok.into());
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
            set(&obj, "status", d.status.into());
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
        MessageBody::FlowUpdateRequestBody(r) => {
            set(&obj, "variant", "FlowUpdateRequest".into());
            set(&obj, "flowId", r.flow_id.into());
            if let Some(n) = r.name {
                set(&obj, "name", n.into());
            }
            if let Some(d) = r.description {
                set(&obj, "description", d.into());
            }
            if let Some(fj) = r.flow_json {
                set(&obj, "flowJson", fj.into());
            }
            if let Some(s) = r.status {
                set(&obj, "status", s.into());
            }
        }
        MessageBody::FlowUpdateResponseBody(r) => {
            set(&obj, "variant", "FlowUpdateResponse".into());
            set(&obj, "ok", r.ok.into());
        }
        MessageBody::FlowNodeTemplatesListRequest => {
            set(&obj, "variant", "FlowNodeTemplatesListRequest".into());
        }
        MessageBody::FlowNodeTemplatesListResponseBody(resp) => {
            set(&obj, "variant", "FlowNodeTemplatesListResponse".into());
            let arr = js_sys::Array::new();
            for t in resp.templates {
                arr.push(&flow_node_template_to_js(t).into());
            }
            set(&obj, "templates", arr.into());
        }
        MessageBody::FlowVersionListRequestBody(r) => {
            set(&obj, "variant", "FlowVersionListRequest".into());
            set(&obj, "flowId", r.flow_id.into());
        }
        MessageBody::FlowVersionListResponseBody(resp) => {
            set(&obj, "variant", "FlowVersionListResponse".into());
            let arr = js_sys::Array::new();
            for v in resp.versions {
                arr.push(&flow_version_summary_to_js(v).into());
            }
            set(&obj, "versions", arr.into());
        }
        MessageBody::FlowVersionGetRequestBody(r) => {
            set(&obj, "variant", "FlowVersionGetRequest".into());
            set(&obj, "flowId", r.flow_id.into());
            set(&obj, "versionId", r.version_id.into());
        }
        MessageBody::FlowVersionGetResponseBody(resp) => {
            set(&obj, "variant", "FlowVersionGetResponse".into());
            set(&obj, "version", flow_version_full_to_js(resp.version).into());
        }
        MessageBody::FlowVersionRestoreRequestBody(r) => {
            set(&obj, "variant", "FlowVersionRestoreRequest".into());
            set(&obj, "flowId", r.flow_id.into());
            set(&obj, "versionId", r.version_id.into());
        }
        MessageBody::FlowVersionRestoreResponseBody(r) => {
            set(&obj, "variant", "FlowVersionRestoreResponse".into());
            set(&obj, "ok", r.ok.into());
        }
        MessageBody::SsoProvidersListRequest => {
            set(&obj, "variant", "SsoProvidersListRequest".into());
        }
        MessageBody::SsoProvidersListResponseBody(resp) => {
            set(&obj, "variant", "SsoProvidersListResponse".into());
            let arr = js_sys::Array::new();
            for p in resp.providers {
                let item = js_sys::Object::new();
                set(&item, "id", (p.id as f64).into());
                set(&item, "name", p.name.into());
                set(&item, "providerType", p.provider_type.into());
                set(&item, "discoveryUrl", p.discovery_url.into());
                set(&item, "enabled", p.enabled.into());
                set(&item, "autoCreateUsers", p.auto_create_users.into());
                if let Some(g) = p.default_group_id {
                    set(&item, "defaultGroupId", (g as f64).into());
                }
                set(&item, "createdAt", p.created_at.into());
                arr.push(&item.into());
            }
            set(&obj, "providers", arr.into());
        }
        MessageBody::SsoProviderCreateRequestBody(req) => {
            set(&obj, "variant", "SsoProviderCreateRequest".into());
            set(&obj, "name", req.name.into());
            set(&obj, "providerType", req.provider_type.into());
            set(&obj, "clientId", req.client_id.into());
            set(&obj, "clientSecret", "<redacted>".into());
            set(&obj, "discoveryUrl", req.discovery_url.into());
            set(&obj, "autoCreateUsers", req.auto_create_users.into());
            if let Some(g) = req.default_group_id {
                set(&obj, "defaultGroupId", (g as f64).into());
            }
        }
        MessageBody::SsoProviderCreateResponseBody(resp) => {
            set(&obj, "variant", "SsoProviderCreateResponse".into());
            set(&obj, "id", (resp.id as f64).into());
            set(&obj, "name", resp.name.into());
            set(&obj, "providerType", resp.provider_type.into());
        }
        MessageBody::SsoProviderDeleteRequestBody(req) => {
            set(&obj, "variant", "SsoProviderDeleteRequest".into());
            set(&obj, "id", (req.id as f64).into());
        }
        MessageBody::SsoProviderDeleteResponseBody(resp) => {
            set(&obj, "variant", "SsoProviderDeleteResponse".into());
            set(&obj, "deleted", resp.deleted.into());
        }
        MessageBody::TlsStatusRequest => {
            set(&obj, "variant", "TlsStatusRequest".into());
        }
        MessageBody::TlsStatusResponseBody(resp) => {
            set(&obj, "variant", "TlsStatusResponse".into());
            set(&obj, "hasCert", resp.has_cert.into());
            set(&obj, "hasKey", resp.has_key.into());
        }
        MessageBody::NgcStatusRequest => {
            set(&obj, "variant", "NgcStatusRequest".into());
        }
        MessageBody::NgcStatusResponseBody(resp) => {
            set(&obj, "variant", "NgcStatusResponse".into());
            set(&obj, "configured", resp.configured.into());
        }
        MessageBody::NimCatalogListRequest => {
            set(&obj, "variant", "NimCatalogListRequest".into());
        }
        MessageBody::NimCatalogListResponseBody(resp) => {
            set(&obj, "variant", "NimCatalogListResponse".into());
            let arr = js_sys::Array::new();
            for c in resp.containers {
                let item = js_sys::Object::new();
                set(&item, "name", c.name.into());
                set(&item, "displayName", c.display_name.into());
                set(&item, "description", c.description.into());
                set(&item, "image", c.image.into());
                set(&item, "latestTag", c.latest_tag.into());
                set(&item, "publisher", c.publisher.into());
                set(&item, "category", c.category.into());
                if let Some(mem) = c.min_gpu_memory_gb {
                    set(&item, "minGpuMemoryGb", (mem as f64).into());
                }
                if let Some(at) = c.updated_at {
                    set(&item, "updatedAt", at.into());
                }
                set(&item, "selfHostable", c.self_hostable.into());
                arr.push(&item.into());
            }
            set(&obj, "containers", arr.into());
            if let Some(err) = resp.error {
                set(&obj, "error", err.into());
            }
        }
        MessageBody::DeploymentBody(p) => {
            deployment_payload_to_js(&obj, p);
        }
        // ---- Addons + Users (FAZA 6) ----
        MessageBody::AddonsListRequest => {
            set(&obj, "variant", "AddonsListRequest".into());
        }
        MessageBody::AddonsListResponseBody(resp) => {
            set(&obj, "variant", "AddonsListResponse".into());
            let arr = js_sys::Array::new();
            for a in resp.addons {
                let item = js_sys::Object::new();
                set(&item, "addonId", a.addon_id.into());
                set(&item, "name", a.name.into());
                set(&item, "version", a.version.into());
                set(&item, "description", a.description.into());
                set(&item, "author", a.author.into());
                set(&item, "isEnabled", a.is_enabled.into());
                set(&item, "isSystem", a.is_system.into());
                set(&item, "runtime", a.runtime.into());
                if let Some(m) = a.oauth_mode {
                    set(&item, "oauthMode", m.into());
                } else {
                    set(&item, "oauthMode", JsValue::NULL);
                }
                set(&item, "visibilityScope", a.visibility_scope.into());
                set(
                    &item,
                    "declaredPermissionsCount",
                    (a.declared_permissions_count as f64).into(),
                );
                set(
                    &item,
                    "usersWithOauthCount",
                    (a.users_with_oauth_count as f64).into(),
                );
                if let Some(v) = a.icon {
                    set(&item, "icon", v.into());
                } else {
                    set(&item, "icon", JsValue::NULL);
                }
                if let Some(v) = a.category {
                    set(&item, "category", v.into());
                } else {
                    set(&item, "category", JsValue::NULL);
                }
                set(&item, "fileSizeBytes", (a.file_size_bytes as f64).into());
                arr.push(&item.into());
            }
            set(&obj, "addons", arr.into());
        }
        MessageBody::IamBody(p) => {
            use tentaflow_protocol::IamPayload as IP;
            match p {
                IP::ReqListUsers => set(&obj, "variant", "IamListUsersRequest".into()),
                IP::ResListUsers { users } => {
                    set(&obj, "variant", "IamListUsersResponse".into());
                    let arr = js_sys::Array::new();
                    for u in users.iter() {
                        arr.push(&user_info_to_js(u).into());
                    }
                    set(&obj, "users", arr.into());
                }
                IP::ReqGetUser { user_id } => {
                    set(&obj, "variant", "IamGetUserRequest".into());
                    set(&obj, "userId", (user_id as f64).into());
                }
                IP::ResGetUser { user } => {
                    set(&obj, "variant", "IamGetUserResponse".into());
                    set(&obj, "user", user_info_to_js(&user).into());
                }
                IP::ReqCreateUser { .. } => set(&obj, "variant", "IamCreateUserRequest".into()),
                IP::ResCreateUser { user_id } => {
                    set(&obj, "variant", "IamCreateUserResponse".into());
                    set(&obj, "userId", (user_id as f64).into());
                }
                IP::ReqUpdateUser { .. } => set(&obj, "variant", "IamUpdateUserRequest".into()),
                IP::ReqDeleteUser { .. } => set(&obj, "variant", "IamDeleteUserRequest".into()),
                IP::ReqSetUserGroups { .. } => set(&obj, "variant", "IamSetUserGroupsRequest".into()),
                IP::ReqResetUserPassword { .. } => set(&obj, "variant", "IamResetUserPasswordRequest".into()),
                IP::ReqListGroups => set(&obj, "variant", "IamListGroupsRequest".into()),
                IP::ResListGroups { groups } => {
                    set(&obj, "variant", "IamListGroupsResponse".into());
                    let arr = js_sys::Array::new();
                    for g in groups {
                        let item = js_sys::Object::new();
                        set(&item, "id", (g.id as f64).into());
                        set(&item, "name", g.name.clone().into());
                        set(&item, "description", g.description.clone().into());
                        set(&item, "memberCount", (g.member_count as f64).into());
                        set(&item, "member_count", (g.member_count as f64).into());
                        arr.push(&item.into());
                    }
                    set(&obj, "groups", arr.into());
                }
                IP::ReqCreateGroup { .. } => set(&obj, "variant", "IamCreateGroupRequest".into()),
                IP::ResCreateGroup { group_id } => {
                    set(&obj, "variant", "IamCreateGroupResponse".into());
                    set(&obj, "groupId", (group_id as f64).into());
                }
                IP::ReqUpdateGroup { .. } => set(&obj, "variant", "IamUpdateGroupRequest".into()),
                IP::ReqDeleteGroup { .. } => set(&obj, "variant", "IamDeleteGroupRequest".into()),
                IP::ReqGroupMembers { .. } => set(&obj, "variant", "IamGroupMembersRequest".into()),
                IP::ResGroupMembers { members } => {
                    set(&obj, "variant", "IamGroupMembersResponse".into());
                    let arr = js_sys::Array::new();
                    for u in members.iter() { arr.push(&user_info_to_js(u).into()); }
                    set(&obj, "members", arr.into());
                }
                IP::ReqSetPermission { .. } => set(&obj, "variant", "IamSetPermissionRequest".into()),
                IP::ReqClearPermission { .. } => set(&obj, "variant", "IamClearPermissionRequest".into()),
                IP::ReqListPermsForResource { .. } => set(&obj, "variant", "IamListPermsForResourceRequest".into()),
                IP::ReqListPermsForSubject { .. } => set(&obj, "variant", "IamListPermsForSubjectRequest".into()),
                IP::ResListPermissions { entries } => {
                    set(&obj, "variant", "IamListPermissionsResponse".into());
                    let arr = js_sys::Array::new();
                    for e in entries {
                        let item = js_sys::Object::new();
                        set(&item, "resourceType", e.resource_type.clone().into());
                        set(&item, "resource_type", e.resource_type.clone().into());
                        set(&item, "resourceId", e.resource_id.clone().into());
                        set(&item, "resource_id", e.resource_id.clone().into());
                        set(&item, "subjectType", e.subject_type.clone().into());
                        set(&item, "subject_type", e.subject_type.clone().into());
                        set(&item, "subjectId", (e.subject_id as f64).into());
                        set(&item, "subject_id", (e.subject_id as f64).into());
                        set(&item, "accessLevel", e.access_level.clone().into());
                        set(&item, "access_level", e.access_level.clone().into());
                        arr.push(&item.into());
                    }
                    set(&obj, "entries", arr.into());
                }
                IP::ResOk => set(&obj, "variant", "IamOkResponse".into()),
            }
        }

        // ---- Audit log screen ----
        MessageBody::AuditLogListRequestBody(_) => {
            set(&obj, "variant", "AuditLogListRequest".into());
        }
        MessageBody::AuditLogListResponseBody(resp) => {
            set(&obj, "variant", "AuditLogListResponse".into());
            let arr = js_sys::Array::new();
            for e in resp.entries {
                let item = js_sys::Object::new();
                set(&item, "id", (e.id as f64).into());
                set(&item, "timestamp", e.timestamp.into());
                set(&item, "action", e.action.into());
                if let Some(uid) = e.user_id {
                    set(&item, "userId", (uid as f64).into());
                }
                if let Some(aid) = e.addon_id {
                    set(&item, "addonId", aid.into());
                }
                if let Some(r) = e.resource {
                    set(&item, "resource", r.into());
                }
                if let Some(d) = e.details {
                    set(&item, "details", d.into());
                }
                if let Some(ip) = e.ip_address {
                    set(&item, "ipAddress", ip.into());
                }
                if let Some(n) = e.node_id {
                    set(&item, "nodeId", n.into());
                }
                arr.push(&item.into());
            }
            set(&obj, "entries", arr.into());
            set(&obj, "totalCount", (resp.total_count as f64).into());
        }
        MessageBody::AuditLogExportRequestBody(_) => {
            set(&obj, "variant", "AuditLogExportRequest".into());
        }
        MessageBody::AuditLogExportResponseBody(resp) => {
            set(&obj, "variant", "AuditLogExportResponse".into());
            set(&obj, "csv", resp.csv.into());
            set(&obj, "rowCount", (resp.row_count as f64).into());
        }
        MessageBody::AuditLogCleanupRequestBody(req) => {
            set(&obj, "variant", "AuditLogCleanupRequest".into());
            set(&obj, "keepDays", (req.keep_days as f64).into());
        }
        MessageBody::AuditLogCleanupResponseBody(resp) => {
            set(&obj, "variant", "AuditLogCleanupResponse".into());
            set(&obj, "deletedCount", (resp.deleted_count as f64).into());
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
                set(&item, "name", s.name.into());
                set(&item, "serviceType", s.service_type.into());
                set(&item, "strategy", s.strategy.into());
                set(&item, "status", s.status.into());
                set(&item, "configJson", s.config_json.into());
                set(&item, "createdAt", s.created_at.into());
                if let Some(nid) = s.node_id {
                    set(&item, "nodeId", nid.into());
                }
                if let Some(host) = s.node_hostname {
                    set(&item, "nodeHostname", host.into());
                }
                if let Some(method) = s.deploy_method {
                    set(&item, "deployMethod", method.into());
                }
                if let Some(url) = s.endpoint_url {
                    set(&item, "endpointUrl", url.into());
                }
                if let Some(t) = s.started_at_epoch {
                    set(&item, "startedAtEpoch", t.into());
                }
                if let Some(eid) = s.engine_id {
                    set(&item, "engineId", eid.into());
                }
                if let Some(mid) = s.model_id {
                    set(&item, "modelId", mid.into());
                }
                set(&item, "pinned", s.pinned.into());
                set(&item, "paused", s.paused.into());
                if let Some(h) = s.deployed_source_hash {
                    set(&item, "deployedSourceHash", h.into());
                }
                arr.push(&item.into());
            }
            set(&obj, "services", arr.into());
        }
        MessageBody::ServiceCreateRequestBody(req) => {
            set(&obj, "variant", "ServiceCreateRequest".into());
            set(&obj, "name", req.name.into());
            set(&obj, "serviceType", req.service_type.into());
            set(&obj, "strategy", req.strategy.into());
            set(&obj, "configJson", req.config_json.into());
            if let Some(nid) = req.node_id {
                set(&obj, "nodeId", nid.into());
            }
            if let Some(cid) = req.cluster_id {
                set(&obj, "clusterId", cid.into());
            }
        }
        MessageBody::ServiceCreateResponse { id } => {
            set(&obj, "variant", "ServiceCreateResponse".into());
            set(&obj, "id", id.into());
        }
        MessageBody::ServiceUpdateRequestBody(req) => {
            set(&obj, "variant", "ServiceUpdateRequest".into());
            set(&obj, "id", req.id.into());
            set(&obj, "name", req.name.into());
            set(&obj, "serviceType", req.service_type.into());
            set(&obj, "strategy", req.strategy.into());
            set(&obj, "status", req.status.into());
            set(&obj, "configJson", req.config_json.into());
            if let Some(nid) = req.node_id {
                set(&obj, "nodeId", nid.into());
            }
            if let Some(cid) = req.cluster_id {
                set(&obj, "clusterId", cid.into());
            }
        }
        MessageBody::ServiceUpdateResponse { updated } => {
            set(&obj, "variant", "ServiceUpdateResponse".into());
            set(&obj, "updated", updated.into());
        }
        MessageBody::ServiceQuicStatusRequest => {
            set(&obj, "variant", "ServiceQuicStatusRequest".into());
        }
        MessageBody::ServiceQuicStatusResponse { statuses } => {
            set(&obj, "variant", "ServiceQuicStatusResponse".into());
            let arr = js_sys::Array::new();
            for st in statuses {
                let item = js_sys::Object::new();
                set(&item, "name", st.name.into());
                set(&item, "status", st.status.into());
                arr.push(&item.into());
            }
            set(&obj, "statuses", arr.into());
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
        MessageBody::ServiceFlagsBody(payload) => match payload {
            tentaflow_protocol::ServiceFlagsPayload::Req(r) => {
                set(&obj, "variant", "ServiceFlagsUpdateRequest".into());
                set(&obj, "serviceId", r.service_id.into());
                if let Some(p) = r.pinned {
                    set(&obj, "pinned", p.into());
                }
                if let Some(p) = r.paused {
                    set(&obj, "paused", p.into());
                }
            }
            tentaflow_protocol::ServiceFlagsPayload::Res(r) => {
                set(&obj, "variant", "ServiceFlagsUpdateResponse".into());
                set(&obj, "ok", r.ok.into());
                set(&obj, "pinned", r.pinned.into());
                set(&obj, "paused", r.paused.into());
            }
        },
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
        MessageBody::NotesRequestBody(_) => {
            set(&obj, "variant", "NotesRequest".into());
        }
        MessageBody::NotesResponseBody(r) => match r {
            NotesResponse::List(resp) => {
                set(&obj, "variant", "NotesListResponse".into());
                let arr = js_sys::Array::new();
                for n in resp.notes {
                    let item = js_sys::Object::new();
                    set(&item, "id", (n.id as f64).into());
                    set(&item, "title", n.title.into());
                    set(&item, "bodyPreview", n.body_preview.clone().into());
                    set(&item, "body_preview", n.body_preview.into());
                    set(&item, "pinned", n.pinned.into());
                    set(&item, "createdAtEpoch", (n.created_at_epoch as f64).into());
                    set(&item, "created_at_epoch", (n.created_at_epoch as f64).into());
                    set(&item, "updatedAtEpoch", (n.updated_at_epoch as f64).into());
                    set(&item, "updated_at_epoch", (n.updated_at_epoch as f64).into());
                    arr.push(&item.into());
                }
                set(&obj, "notes", arr.into());
            }
            NotesResponse::Detail(d) => {
                set(&obj, "variant", "NoteDetailResponse".into());
                set(&obj, "id", (d.id as f64).into());
                set(&obj, "title", d.title.into());
                set(&obj, "body", d.body.into());
                set(&obj, "pinned", d.pinned.into());
                set(&obj, "createdAtEpoch", (d.created_at_epoch as f64).into());
                set(&obj, "created_at_epoch", (d.created_at_epoch as f64).into());
                set(&obj, "updatedAtEpoch", (d.updated_at_epoch as f64).into());
                set(&obj, "updated_at_epoch", (d.updated_at_epoch as f64).into());
            }
            NotesResponse::Create(c) => {
                set(&obj, "variant", "NoteCreateResponse".into());
                set(&obj, "id", (c.id as f64).into());
            }
            NotesResponse::Update(u) => {
                set(&obj, "variant", "NoteUpdateResponse".into());
                set(&obj, "ok", u.ok.into());
                set(&obj, "updatedAtEpoch", (u.updated_at_epoch as f64).into());
                set(&obj, "updated_at_epoch", (u.updated_at_epoch as f64).into());
            }
            NotesResponse::SetPinned(p) => {
                set(&obj, "variant", "NoteSetPinnedResponse".into());
                set(&obj, "ok", p.ok.into());
            }
            NotesResponse::Delete(d) => {
                set(&obj, "variant", "NoteDeleteResponse".into());
                set(&obj, "ok", d.ok.into());
            }
        },
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
        // Pelne CRUD klastrow + member ops + probe streaming. Decoder eksponuje pola
        // jako properties JS objektu (camelCase), enum stringi 1:1 z server-side.
        MessageBody::ClusterListRequest => {
            set(&obj, "variant", "ClusterListRequest".into());
        }
        MessageBody::ClusterListResponseBody(resp) => {
            set(&obj, "variant", "ClusterListResponse".into());
            let arr = js_sys::Array::new();
            for c in resp.clusters {
                arr.push(&cluster_info_to_js(c).into());
            }
            set(&obj, "clusters", arr.into());
        }
        MessageBody::ClusterDetailRequestBody(req) => {
            set(&obj, "variant", "ClusterDetailRequest".into());
            set(&obj, "clusterId", req.cluster_id.into());
        }
        MessageBody::ClusterDetailResponseBody(resp) => {
            set(&obj, "variant", "ClusterDetailResponse".into());
            set(&obj, "cluster", cluster_info_to_js(resp.cluster).into());
            let arr = js_sys::Array::new();
            for m in resp.members {
                let item = js_sys::Object::new();
                set(&item, "nodeId", m.node_id.into());
                set(&item, "hostname", m.hostname.into());
                set(&item, "status", m.status.into());
                if let Some(t) = m.interface_type { set(&item, "interfaceType", t.into()); }
                if let Some(s) = m.interface_speed_mbps { set(&item, "interfaceSpeedMbps", s.into()); }
                set(&item, "joinedAt", (m.joined_at as f64).into());
                arr.push(&item.into());
            }
            set(&obj, "members", arr.into());
        }
        MessageBody::ClusterCreateRequestBody(req) => {
            set(&obj, "variant", "ClusterCreateRequest".into());
            set(&obj, "name", req.name.into());
            if let Some(d) = req.description { set(&obj, "description", d.into()); }
            set(&obj, "strategy", req.strategy.into());
            set(&obj, "failoverEnabled", req.failover_enabled.into());
            if let Some(t) = req.failover_target { set(&obj, "failoverTarget", t.into()); }
            set(&obj, "healthCheckIntervalMs", req.health_check_interval_ms.into());
            set(&obj, "timeoutMs", req.timeout_ms.into());
        }
        MessageBody::ClusterCreateResponseBody(resp) => {
            set(&obj, "variant", "ClusterCreateResponse".into());
            set(&obj, "clusterId", resp.cluster_id.into());
        }
        MessageBody::ClusterDeleteRequestBody(req) => {
            set(&obj, "variant", "ClusterDeleteRequest".into());
            set(&obj, "clusterId", req.cluster_id.into());
        }
        MessageBody::ClusterDeleteResponseBody(resp) => {
            set(&obj, "variant", "ClusterDeleteResponse".into());
            set(&obj, "ok", resp.ok.into());
        }
        MessageBody::ClusterAddMemberRequestBody(req) => {
            set(&obj, "variant", "ClusterAddMemberRequest".into());
            set(&obj, "clusterId", req.cluster_id.into());
            set(&obj, "nodeId", req.node_id.into());
            if let Some(t) = req.interface_type { set(&obj, "interfaceType", t.into()); }
            if let Some(s) = req.interface_speed_mbps { set(&obj, "interfaceSpeedMbps", s.into()); }
        }
        MessageBody::ClusterAddMemberResponseBody(resp) => {
            set(&obj, "variant", "ClusterAddMemberResponse".into());
            set(&obj, "ok", resp.ok.into());
        }
        MessageBody::ClusterRemoveMemberRequestBody(req) => {
            set(&obj, "variant", "ClusterRemoveMemberRequest".into());
            set(&obj, "clusterId", req.cluster_id.into());
            set(&obj, "nodeId", req.node_id.into());
        }
        MessageBody::ClusterRemoveMemberResponseBody(resp) => {
            set(&obj, "variant", "ClusterRemoveMemberResponse".into());
            set(&obj, "ok", resp.ok.into());
        }
        MessageBody::ClusterProbeStreamRequestBody(req) => {
            set(&obj, "variant", "ClusterProbeStreamRequest".into());
            let arr = js_sys::Array::new();
            for n in req.node_ids { arr.push(&n.into()); }
            set(&obj, "nodeIds", arr.into());
        }
        MessageBody::ClusterProbeStreamChunkBody(c) => {
            set(&obj, "variant", "ClusterProbeStreamChunk".into());
            set(&obj, "eventType", c.event_type.into());
            if let Some(s) = c.source_node { set(&obj, "sourceNode", s.into()); }
            if let Some(t) = c.target_node { set(&obj, "targetNode", t.into()); }
            if let Some(s) = c.success { set(&obj, "success", s.into()); }
            if let Some(v) = c.latency_ms { set(&obj, "latencyMs", v.into()); }
            if let Some(v) = c.bandwidth_mbps { set(&obj, "bandwidthMbps", v.into()); }
            if let Some(t) = c.interface_type { set(&obj, "interfaceType", t.into()); }
            if let Some(m) = c.message { set(&obj, "message", m.into()); }
        }
        MessageBody::ClusterProbeStreamEndBody(e) => {
            set(&obj, "variant", "ClusterProbeStreamEnd".into());
            set(&obj, "totalPairs", e.total_pairs.into());
            set(&obj, "successful", e.successful.into());
            set(&obj, "failed", e.failed.into());
        }
        // ---- Mesh read-only (FAZA 1a) ----
        MessageBody::MeshNodeListRequest => {
            set(&obj, "variant", "MeshNodeListRequest".into());
        }
        MessageBody::MeshNodeListResponseBody(resp) => {
            set(&obj, "variant", "MeshNodeListResponse".into());
            let arr = js_sys::Array::new();
            for n in resp.nodes {
                arr.push(&mesh_node_info_to_js(n).into());
            }
            set(&obj, "nodes", arr.into());
        }
        MessageBody::MeshNodeDetailRequestBody(req) => {
            set(&obj, "variant", "MeshNodeDetailRequest".into());
            set(&obj, "nodeId", req.node_id.into());
        }
        MessageBody::MeshNodeDetailResponseBody(resp) => {
            set(&obj, "variant", "MeshNodeDetailResponse".into());
            set(&obj, "node", mesh_node_info_to_js(resp.node).into());
        }
        MessageBody::MeshPendingListRequest => {
            set(&obj, "variant", "MeshPendingListRequest".into());
        }
        MessageBody::MeshPendingListResponseBody(resp) => {
            set(&obj, "variant", "MeshPendingListResponse".into());
            let arr = js_sys::Array::new();
            for p in resp.pending {
                let item = js_sys::Object::new();
                set(&item, "pairId", p.pair_id.into());
                set(&item, "remoteNodeId", p.remote_node_id.into());
                if let Some(h) = p.remote_hostname {
                    set(&item, "remoteHostname", h.into());
                }
                if let Some(ip) = p.remote_ip {
                    set(&item, "remoteIp", ip.into());
                }
                set(&item, "initiatedAt", (p.initiated_at as f64).into());
                set(&item, "state", p.state.into());
                if let Some(pin) = p.pin {
                    set(&item, "pin", pin.into());
                }
                arr.push(&item.into());
            }
            set(&obj, "pending", arr.into());
        }
        MessageBody::MeshIdentityRequest => {
            set(&obj, "variant", "MeshIdentityRequest".into());
        }
        MessageBody::MeshIdentityResponseBody(resp) => {
            set(&obj, "variant", "MeshIdentityResponse".into());
            set(&obj, "nodeId", resp.node_id.clone().into());
            set(&obj, "node_id", resp.node_id.into());
            set(&obj, "hostname", resp.hostname.into());
            set(&obj, "publicKey", resp.public_key.into());
            let addrs = js_sys::Array::new();
            for a in resp.addresses {
                addrs.push(&a.into());
            }
            set(&obj, "addresses", addrs.into());
            set(&obj, "relayUrl", resp.relay_url.clone().into());
            set(&obj, "relay_url", resp.relay_url.into());
            set(&obj, "version", resp.version.into());
            set(&obj, "invitePin", resp.invite_pin.clone().into());
            set(&obj, "invite_pin", resp.invite_pin.into());
            set(&obj, "invitePinExpiresSec", (resp.invite_pin_expires_sec as f64).into());
            set(&obj, "invite_pin_expires_sec", (resp.invite_pin_expires_sec as f64).into());
        }
        MessageBody::MeshServicesListRequest => {
            set(&obj, "variant", "MeshServicesListRequest".into());
        }
        MessageBody::MeshServicesListResponseBody(resp) => {
            set(&obj, "variant", "MeshServicesListResponse".into());
            let arr = js_sys::Array::new();
            for s in resp.services {
                let item = js_sys::Object::new();
                set(&item, "serviceName", s.service_name.into());
                set(&item, "nodeId", s.node_id.into());
                set(&item, "status", s.status.into());
                if let Some(e) = s.endpoint {
                    set(&item, "endpoint", e.into());
                }
                arr.push(&item.into());
            }
            set(&obj, "services", arr.into());
        }
        MessageBody::MeshTrustedListRequest => {
            set(&obj, "variant", "MeshTrustedListRequest".into());
        }
        MessageBody::MeshTrustedListResponseBody(resp) => {
            set(&obj, "variant", "MeshTrustedListResponse".into());
            let arr = js_sys::Array::new();
            for t in resp.trusted {
                let item = js_sys::Object::new();
                set(&item, "nodeId", t.node_id.into());
                if let Some(h) = t.hostname {
                    set(&item, "hostname", h.into());
                }
                set(&item, "trustedSinceEpoch", (t.trusted_since_epoch as f64).into());
                arr.push(&item.into());
            }
            set(&obj, "trusted", arr.into());
        }
        MessageBody::MeshPairingStartRequestBody(r) => {
            set(&obj, "variant", "MeshPairingStartRequest".into());
            set(&obj, "remoteAddress", r.remote_address.into());
            set(&obj, "pinHint", r.pin_hint.into());
            set(&obj, "remotePublicKey", r.remote_public_key.into());
            let addrs = js_sys::Array::new();
            for a in r.remote_addresses {
                addrs.push(&a.into());
            }
            set(&obj, "remoteAddresses", addrs.into());
            set(&obj, "remoteRelayUrl", r.remote_relay_url.into());
            set(&obj, "remoteHostname", r.remote_hostname.into());
        }
        MessageBody::MeshPairingStartResponseBody(r) => {
            set(&obj, "variant", "MeshPairingStartResponse".into());
            set(&obj, "pairId", r.pair_id.into());
            set(&obj, "pin", r.pin.into());
            set(&obj, "completed", r.completed.into());
        }
        MessageBody::MeshPairingConfirmRequestBody(r) => {
            set(&obj, "variant", "MeshPairingConfirmRequest".into());
            set(&obj, "pairId", r.pair_id.into());
            set(&obj, "pin", r.pin.into());
        }
        MessageBody::MeshPairingConfirmResponseBody(r) => {
            set(&obj, "variant", "MeshPairingConfirmResponse".into());
            set(&obj, "ok", r.ok.into());
            set(&obj, "trustedNodeId", r.trusted_node_id.into());
        }
        MessageBody::MeshPairingRejectRequestBody(r) => {
            set(&obj, "variant", "MeshPairingRejectRequest".into());
            set(&obj, "pairId", r.pair_id.into());
        }
        MessageBody::MeshPairingRejectResponseBody(r) => {
            set(&obj, "variant", "MeshPairingRejectResponse".into());
            set(&obj, "ok", r.ok.into());
        }
        MessageBody::MeshTrustRevokeRequestBody(r) => {
            set(&obj, "variant", "MeshTrustRevokeRequest".into());
            set(&obj, "nodeId", r.node_id.into());
        }
        MessageBody::MeshTrustRevokeResponseBody(r) => {
            set(&obj, "variant", "MeshTrustRevokeResponse".into());
            set(&obj, "ok", r.ok.into());
        }
        MessageBody::MeshTrustRetrustRequestBody(r) => {
            set(&obj, "variant", "MeshTrustRetrustRequest".into());
            set(&obj, "nodeId", r.node_id.into());
        }
        MessageBody::MeshTrustRetrustResponseBody(r) => {
            set(&obj, "variant", "MeshTrustRetrustResponse".into());
            set(&obj, "ok", r.ok.into());
        }
        MessageBody::MeshConnectRequestBody(r) => {
            set(&obj, "variant", "MeshConnectRequest".into());
            set(&obj, "address", r.address.into());
        }
        MessageBody::MeshConnectResponseBody(r) => {
            set(&obj, "variant", "MeshConnectResponse".into());
            set(&obj, "ok", r.ok.into());
            if let Some(id) = r.remote_node_id {
                set(&obj, "remoteNodeId", id.into());
            }
        }
        MessageBody::MeshNodeCommandRequestBody(r) => {
            set(&obj, "variant", "MeshNodeCommandRequest".into());
            set(&obj, "nodeId", r.node_id.into());
            set(&obj, "command", r.command.into());
            let arr = js_sys::Array::new();
            for a in r.args {
                arr.push(&a.into());
            }
            set(&obj, "args", arr.into());
        }
        MessageBody::MeshNodeCommandResponseBody(r) => {
            set(&obj, "variant", "MeshNodeCommandResponse".into());
            set(&obj, "ok", r.ok.into());
            if let Some(out) = r.output {
                set(&obj, "output", out.into());
            }
        }
        MessageBody::MeshNodeNetworkConfigRequestBody(r) => {
            set(&obj, "variant", "MeshNodeNetworkConfigRequest".into());
            set(&obj, "nodeId", r.node_id.into());
            set(&obj, "interfaceName", r.interface_name.into());
            set(&obj, "configJson", r.config_json.into());
        }
        MessageBody::MeshNodeNetworkConfigResponseBody(r) => {
            set(&obj, "variant", "MeshNodeNetworkConfigResponse".into());
            set(&obj, "ok", r.ok.into());
        }
        MessageBody::ModelsUnifiedListRequest => {
            set(&obj, "variant", "ModelsUnifiedListRequest".into());
        }
        MessageBody::ModelsUnifiedListResponseBody(resp) => {
            set(&obj, "variant", "ModelsUnifiedListResponse".into());
            let arr = js_sys::Array::new();
            for m in resp.models {
                let item = js_sys::Object::new();
                set(&item, "modelName", m.model_name.clone().into());
                set(&item, "model_name", m.model_name.into());
                set(&item, "serviceType", m.service_type.clone().into());
                set(&item, "service_type", m.service_type.into());
                let instances = js_sys::Array::new();
                for i in m.instances {
                    let inst = js_sys::Object::new();
                    set(&inst, "nodeId", i.node_id.clone().into());
                    set(&inst, "node_id", i.node_id.into());
                    if let Some(ref h) = i.node_hostname {
                        set(&inst, "nodeHostname", h.clone().into());
                        set(&inst, "node_hostname", h.clone().into());
                        set(&inst, "node_name", h.clone().into());
                    }
                    set(&inst, "serviceId", i.service_id.clone().into());
                    set(&inst, "service_id", i.service_id.into());
                    set(&inst, "status", i.status.clone().into());
                    if let Some(ref b) = i.backend {
                        set(&inst, "backend", b.clone().into());
                    }
                    if let Some(s) = i.size_mb {
                        set(&inst, "sizeMb", (s as f64).into());
                        set(&inst, "size_mb", (s as f64).into());
                    }
                    set(&inst, "loaded", i.loaded.into());
                    instances.push(&inst.into());
                }
                set(&item, "instances", instances.into());
                arr.push(&item.into());
            }
            set(&obj, "models", arr.into());
        }
        MessageBody::ModelAliasListRequest => {
            set(&obj, "variant", "ModelAliasListRequest".into());
        }
        MessageBody::ModelAliasListResponseBody(resp) => {
            set(&obj, "variant", "ModelAliasListResponse".into());
            let arr = js_sys::Array::new();
            for a in resp.aliases {
                arr.push(&model_alias_entry_to_js(a).into());
            }
            set(&obj, "aliases", arr.into());
        }
        MessageBody::ModelAliasCreateRequestBody(r) => {
            set(&obj, "variant", "ModelAliasCreateRequest".into());
            set(&obj, "alias", r.alias.into());
            set(&obj, "targetModel", r.target_model.clone().into());
            set(&obj, "target_model", r.target_model.into());
            if let Some(s) = r.strategy { set(&obj, "strategy", s.into()); }
            if let Some(f) = r.fallback_targets {
                set(&obj, "fallbackTargets", f.clone().into());
                set(&obj, "fallback_targets", f.into());
            }
        }
        MessageBody::ModelAliasCreateResponseBody(r) => {
            set(&obj, "variant", "ModelAliasCreateResponse".into());
            set(&obj, "id", (r.id as f64).into());
        }
        MessageBody::ModelAliasUpdateRequestBody(r) => {
            set(&obj, "variant", "ModelAliasUpdateRequest".into());
            set(&obj, "id", (r.id as f64).into());
            set(&obj, "alias", r.alias.into());
            set(&obj, "targetModel", r.target_model.clone().into());
            set(&obj, "target_model", r.target_model.into());
            if let Some(a) = r.is_active {
                set(&obj, "isActive", a.into());
                set(&obj, "is_active", a.into());
            }
            if let Some(s) = r.strategy { set(&obj, "strategy", s.into()); }
            if let Some(f) = r.fallback_targets {
                set(&obj, "fallbackTargets", f.clone().into());
                set(&obj, "fallback_targets", f.into());
            }
        }
        MessageBody::ModelAliasUpdateResponseBody(r) => {
            set(&obj, "variant", "ModelAliasUpdateResponse".into());
            set(&obj, "ok", r.ok.into());
        }
        MessageBody::ModelAliasDeleteRequestBody(r) => {
            set(&obj, "variant", "ModelAliasDeleteRequest".into());
            set(&obj, "id", (r.id as f64).into());
        }
        MessageBody::ModelAliasDeleteResponseBody(r) => {
            set(&obj, "variant", "ModelAliasDeleteResponse".into());
            set(&obj, "ok", r.ok.into());
        }
        // ---- Addon permissions + OAuth (migracja 38) ----
        MessageBody::AddonDetailRequestBody(req) => {
            set(&obj, "variant", "AddonDetailRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
        }
        MessageBody::AddonDetailResponseBody(resp) => {
            set(&obj, "variant", "AddonDetailResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            set(&obj, "name", resp.name.into());
            set(&obj, "version", resp.version.into());
            set(&obj, "description", resp.description.into());
            set(&obj, "author", resp.author.into());
            set(&obj, "isEnabled", resp.is_enabled.into());
            set(&obj, "is_enabled", resp.is_enabled.into());
            set(&obj, "isSystem", resp.is_system.into());
            set(&obj, "is_system", resp.is_system.into());
            set(&obj, "adminOnly", resp.admin_only.into());
            set(&obj, "admin_only", resp.admin_only.into());
            set(&obj, "category", resp.category.into());
            let perms = js_sys::Array::new();
            for p in resp.permissions {
                perms.push(&addon_permission_decl_to_js(p).into());
            }
            set(&obj, "permissions", perms.into());
            let providers = js_sys::Array::new();
            for pr in resp.oauth_providers {
                providers.push(&addon_oauth_provider_decl_to_js(pr).into());
            }
            set(&obj, "oauthProviders", providers.clone().into());
            set(&obj, "oauth_providers", providers.into());
            set(&obj, "license", resp.license.into());
            set(&obj, "fileSizeBytes", (resp.file_size_bytes as f64).into());
            set(&obj, "file_size_bytes", (resp.file_size_bytes as f64).into());
            set(&obj, "runtime", resp.runtime.into());
            match resp.icon {
                Some(ref v) => set(&obj, "icon", v.clone().into()),
                None => set(&obj, "icon", JsValue::NULL),
            }
            match resp.oauth_mode {
                Some(ref v) => {
                    set(&obj, "oauthMode", v.clone().into());
                    set(&obj, "oauth_mode", v.clone().into());
                }
                None => {
                    set(&obj, "oauthMode", JsValue::NULL);
                    set(&obj, "oauth_mode", JsValue::NULL);
                }
            }
            set(&obj, "visibilityGroupsVisible", (resp.visibility_groups_visible as f64).into());
            set(&obj, "visibility_groups_visible", (resp.visibility_groups_visible as f64).into());
            set(&obj, "visibilityGroupsTotal", (resp.visibility_groups_total as f64).into());
            set(&obj, "visibility_groups_total", (resp.visibility_groups_total as f64).into());
            set(&obj, "toolsCount", (resp.tools_count as f64).into());
            set(&obj, "tools_count", (resp.tools_count as f64).into());
            set(&obj, "linkedAccountsCount", (resp.linked_accounts_count as f64).into());
            set(&obj, "linked_accounts_count", (resp.linked_accounts_count as f64).into());
            set(&obj, "showInCatalog", resp.show_in_catalog.into());
            set(&obj, "show_in_catalog", resp.show_in_catalog.into());
        }
        MessageBody::AddonVisibilityListRequestBody(req) => {
            set(&obj, "variant", "AddonVisibilityListRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
        }
        MessageBody::AddonVisibilityListResponseBody(resp) => {
            set(&obj, "variant", "AddonVisibilityListResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            let arr = js_sys::Array::new();
            for r in resp.rows {
                let item = js_sys::Object::new();
                set(&item, "addonId", r.addon_id.clone().into());
                set(&item, "addon_id", r.addon_id.into());
                set(&item, "groupId", (r.group_id as f64).into());
                set(&item, "group_id", (r.group_id as f64).into());
                set(&item, "groupName", r.group_name.clone().into());
                set(&item, "group_name", r.group_name.into());
                set(&item, "visible", r.visible.into());
                set(&item, "groupDescription", r.group_description.clone().into());
                set(&item, "group_description", r.group_description.into());
                set(&item, "userCount", (r.user_count as f64).into());
                set(&item, "user_count", (r.user_count as f64).into());
                arr.push(&item.into());
            }
            set(&obj, "rows", arr.into());
            set(&obj, "showInCatalog", resp.show_in_catalog.into());
            set(&obj, "show_in_catalog", resp.show_in_catalog.into());
        }
        MessageBody::AddonVisibilitySetRequestBody(req) => {
            set(&obj, "variant", "AddonVisibilitySetRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
            set(&obj, "groupId", (req.group_id as f64).into());
            set(&obj, "group_id", (req.group_id as f64).into());
            set(&obj, "visible", req.visible.into());
        }
        MessageBody::AddonVisibilitySetResponseBody(resp) => {
            set(&obj, "variant", "AddonVisibilitySetResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            set(&obj, "groupId", (resp.group_id as f64).into());
            set(&obj, "group_id", (resp.group_id as f64).into());
            set(&obj, "visible", resp.visible.into());
        }
        MessageBody::AddonAdminOnlySetRequestBody(req) => {
            set(&obj, "variant", "AddonAdminOnlySetRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
            set(&obj, "adminOnly", req.admin_only.into());
            set(&obj, "admin_only", req.admin_only.into());
        }
        MessageBody::AddonAdminOnlySetResponseBody(resp) => {
            set(&obj, "variant", "AddonAdminOnlySetResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            set(&obj, "adminOnly", resp.admin_only.into());
            set(&obj, "admin_only", resp.admin_only.into());
        }
        MessageBody::AddonShowInCatalogSetRequestBody(req) => {
            set(&obj, "variant", "AddonShowInCatalogSetRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
            set(&obj, "showInCatalog", req.show_in_catalog.into());
            set(&obj, "show_in_catalog", req.show_in_catalog.into());
        }
        MessageBody::AddonShowInCatalogSetResponseBody(resp) => {
            set(&obj, "variant", "AddonShowInCatalogSetResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            set(&obj, "showInCatalog", resp.show_in_catalog.into());
            set(&obj, "show_in_catalog", resp.show_in_catalog.into());
        }
        MessageBody::AddonPermissionCatalogRequestBody(req) => {
            set(&obj, "variant", "AddonPermissionCatalogRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
        }
        MessageBody::AddonPermissionCatalogResponseBody(resp) => {
            set(&obj, "variant", "AddonPermissionCatalogResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            let arr = js_sys::Array::new();
            for e in resp.entries {
                arr.push(&addon_permission_decl_to_js(e).into());
            }
            set(&obj, "entries", arr.into());
        }
        MessageBody::AddonPermissionMatrixRequestBody(req) => {
            set(&obj, "variant", "AddonPermissionMatrixRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
        }
        MessageBody::AddonPermissionMatrixResponseBody(resp) => {
            set(&obj, "variant", "AddonPermissionMatrixResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            let rows = js_sys::Array::new();
            for r in resp.rows {
                rows.push(&addon_permission_row_to_js(r).into());
            }
            set(&obj, "rows", rows.into());
            let defs = js_sys::Array::new();
            for d in resp.defaults {
                defs.push(&addon_permission_default_to_js(d).into());
            }
            set(&obj, "defaults", defs.into());
            set(&obj, "lastChangeBy", resp.last_change_by.clone().into());
            set(&obj, "last_change_by", resp.last_change_by.into());
            set(&obj, "lastChangeAtEpoch", (resp.last_change_at_epoch as f64).into());
            set(&obj, "last_change_at_epoch", (resp.last_change_at_epoch as f64).into());
        }
        MessageBody::AddonPermissionSetRequestBody(req) => {
            set(&obj, "variant", "AddonPermissionSetRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
            set(&obj, "subjectType", req.subject_type.clone().into());
            set(&obj, "subject_type", req.subject_type.into());
            set(&obj, "subjectId", (req.subject_id as f64).into());
            set(&obj, "subject_id", (req.subject_id as f64).into());
            set(&obj, "permissionId", req.permission_id.clone().into());
            set(&obj, "permission_id", req.permission_id.into());
            set(&obj, "grantMode", req.grant_mode.clone().into());
            set(&obj, "grant_mode", req.grant_mode.into());
        }
        MessageBody::AddonPermissionSetResponseBody(resp) => {
            set(&obj, "variant", "AddonPermissionSetResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            set(&obj, "subjectType", resp.subject_type.clone().into());
            set(&obj, "subject_type", resp.subject_type.into());
            set(&obj, "subjectId", (resp.subject_id as f64).into());
            set(&obj, "subject_id", (resp.subject_id as f64).into());
            set(&obj, "permissionId", resp.permission_id.clone().into());
            set(&obj, "permission_id", resp.permission_id.into());
            set(&obj, "grantMode", resp.grant_mode.clone().into());
            set(&obj, "grant_mode", resp.grant_mode.into());
        }
        MessageBody::AddonPermissionDefaultSetRequestBody(req) => {
            set(&obj, "variant", "AddonPermissionDefaultSetRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
            set(&obj, "permissionId", req.permission_id.clone().into());
            set(&obj, "permission_id", req.permission_id.into());
            set(&obj, "grantMode", req.grant_mode.clone().into());
            set(&obj, "grant_mode", req.grant_mode.into());
        }
        MessageBody::AddonPermissionDefaultSetResponseBody(resp) => {
            set(&obj, "variant", "AddonPermissionDefaultSetResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            set(&obj, "permissionId", resp.permission_id.clone().into());
            set(&obj, "permission_id", resp.permission_id.into());
            set(&obj, "grantMode", resp.grant_mode.clone().into());
            set(&obj, "grant_mode", resp.grant_mode.into());
        }
        MessageBody::AddonPermissionCheckRequestBody(req) => {
            set(&obj, "variant", "AddonPermissionCheckRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
            set(&obj, "permissionId", req.permission_id.clone().into());
            set(&obj, "permission_id", req.permission_id.into());
            if let Some(uid) = req.user_id {
                set(&obj, "userId", (uid as f64).into());
                set(&obj, "user_id", (uid as f64).into());
            }
        }
        MessageBody::AddonPermissionCheckResponseBody(resp) => {
            set(&obj, "variant", "AddonPermissionCheckResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            set(&obj, "permissionId", resp.permission_id.clone().into());
            set(&obj, "permission_id", resp.permission_id.into());
            set(&obj, "allowed", resp.allowed.into());
            set(&obj, "reason", resp.reason.into());
        }
        MessageBody::AddonOAuthConfigListRequestBody(req) => {
            set(&obj, "variant", "AddonOAuthConfigListRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
        }
        MessageBody::AddonOAuthConfigListResponseBody(resp) => {
            set(&obj, "variant", "AddonOAuthConfigListResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            let arr = js_sys::Array::new();
            for c in resp.configs {
                arr.push(&addon_oauth_config_row_to_js(c).into());
            }
            set(&obj, "configs", arr.into());
        }
        MessageBody::AddonOAuthConfigSetRequestBody(req) => {
            set(&obj, "variant", "AddonOAuthConfigSetRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
            set(&obj, "providerId", req.provider_id.clone().into());
            set(&obj, "provider_id", req.provider_id.into());
            set(&obj, "clientId", req.client_id.clone().into());
            set(&obj, "client_id", req.client_id.into());
            // Secret NIGDY nie odslaniamy w decode (logi/devtools).
            set(&obj, "clientSecret", "<redacted>".into());
            set(&obj, "client_secret", "<redacted>".into());
            set(&obj, "redirectUri", req.redirect_uri.clone().into());
            set(&obj, "redirect_uri", req.redirect_uri.into());
            set(&obj, "enabled", req.enabled.into());
            set(&obj, "oauthMode", req.oauth_mode.clone().into());
            set(&obj, "oauth_mode", req.oauth_mode.into());
        }
        MessageBody::AddonOAuthConfigSetResponseBody(resp) => {
            set(&obj, "variant", "AddonOAuthConfigSetResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            set(&obj, "providerId", resp.provider_id.clone().into());
            set(&obj, "provider_id", resp.provider_id.into());
            set(&obj, "clientSecretSet", resp.client_secret_set.into());
            set(&obj, "client_secret_set", resp.client_secret_set.into());
            set(&obj, "enabled", resp.enabled.into());
        }
        MessageBody::AddonOAuthConfigClearSecretRequestBody(req) => {
            set(&obj, "variant", "AddonOAuthConfigClearSecretRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
            set(&obj, "providerId", req.provider_id.clone().into());
            set(&obj, "provider_id", req.provider_id.into());
        }
        MessageBody::AddonOAuthConfigClearSecretResponseBody(resp) => {
            set(&obj, "variant", "AddonOAuthConfigClearSecretResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            set(&obj, "providerId", resp.provider_id.clone().into());
            set(&obj, "provider_id", resp.provider_id.into());
            set(&obj, "cleared", resp.cleared.into());
        }
        MessageBody::AddonOAuthAuthorizeStartRequestBody(req) => {
            set(&obj, "variant", "AddonOAuthAuthorizeStartRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
            set(&obj, "providerId", req.provider_id.clone().into());
            set(&obj, "provider_id", req.provider_id.into());
            set(&obj, "mode", req.mode.into());
            if let Some(r) = req.redirect_after {
                set(&obj, "redirectAfter", r.clone().into());
                set(&obj, "redirect_after", r.into());
            }
        }
        MessageBody::AddonOAuthAuthorizeStartResponseBody(resp) => {
            set(&obj, "variant", "AddonOAuthAuthorizeStartResponse".into());
            set(&obj, "authorizeUrl", resp.authorize_url.clone().into());
            set(&obj, "authorize_url", resp.authorize_url.into());
            set(&obj, "state", resp.state.into());
        }
        MessageBody::AddonOAuthLinkedAccountsRequestBody(req) => {
            set(&obj, "variant", "AddonOAuthLinkedAccountsRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
            set(&obj, "scope", req.scope.into());
        }
        MessageBody::AddonOAuthLinkedAccountsResponseBody(resp) => {
            set(&obj, "variant", "AddonOAuthLinkedAccountsResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            let arr = js_sys::Array::new();
            for a in resp.accounts {
                arr.push(&user_oauth_account_row_to_js(a).into());
            }
            set(&obj, "accounts", arr.into());
        }
        MessageBody::AddonOAuthRevokeRequestBody(req) => {
            set(&obj, "variant", "AddonOAuthRevokeRequest".into());
            set(&obj, "accountId", (req.account_id as f64).into());
            set(&obj, "account_id", (req.account_id as f64).into());
        }
        MessageBody::AddonOAuthRevokeResponseBody(resp) => {
            set(&obj, "variant", "AddonOAuthRevokeResponse".into());
            set(&obj, "accountId", (resp.account_id as f64).into());
            set(&obj, "account_id", (resp.account_id as f64).into());
            set(&obj, "revoked", resp.revoked.into());
        }
        MessageBody::AddonOAuthReauthorizeRequestBody(req) => {
            set(&obj, "variant", "AddonOAuthReauthorizeRequest".into());
            set(&obj, "accountId", (req.account_id as f64).into());
            set(&obj, "account_id", (req.account_id as f64).into());
        }
        MessageBody::AddonOAuthReauthorizeResponseBody(resp) => {
            set(&obj, "variant", "AddonOAuthReauthorizeResponse".into());
            set(&obj, "authorizeUrl", resp.authorize_url.clone().into());
            set(&obj, "authorize_url", resp.authorize_url.into());
            set(&obj, "state", resp.state.into());
        }
        MessageBody::AddonOAuthTestConnectionRequestBody(req) => {
            set(&obj, "variant", "AddonOAuthTestConnectionRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
            set(&obj, "providerId", req.provider_id.clone().into());
            set(&obj, "provider_id", req.provider_id.into());
        }
        MessageBody::AddonOAuthTestConnectionResponseBody(resp) => {
            set(&obj, "variant", "AddonOAuthTestConnectionResponse".into());
            set(&obj, "ok", resp.ok.into());
            if let Some(m) = resp.message {
                set(&obj, "message", m.into());
            } else {
                set(&obj, "message", JsValue::NULL);
            }
            if let Some(e) = resp.account_email {
                set(&obj, "accountEmail", e.clone().into());
                set(&obj, "account_email", e.into());
            } else {
                set(&obj, "accountEmail", JsValue::NULL);
                set(&obj, "account_email", JsValue::NULL);
            }
        }
        MessageBody::MyOAuthAccountsListRequestBody(_) => {
            set(&obj, "variant", "MyOAuthAccountsListRequest".into());
        }
        MessageBody::MyOAuthAccountsListResponseBody(resp) => {
            set(&obj, "variant", "MyOAuthAccountsListResponse".into());
            let arr = js_sys::Array::new();
            for e in resp.accounts {
                arr.push(&my_oauth_entry_to_js(e).into());
            }
            set(&obj, "accounts", arr.into());
        }
        MessageBody::SystemEventBody(evt) => {
            match evt {
                tentaflow_protocol::SystemEventPayload::ServiceStatusChanged {
                    service_name,
                    service_type,
                    status,
                    message,
                } => {
                    set(&obj, "variant", "ServiceStatusChanged".into());
                    set(&obj, "serviceName", service_name.clone().into());
                    set(&obj, "service_name", service_name.into());
                    set(&obj, "serviceType", service_type.clone().into());
                    set(&obj, "service_type", service_type.into());
                    set(&obj, "status", status.into());
                    set(&obj, "message", message.into());
                }
                tentaflow_protocol::SystemEventPayload::MeshPeerStatusChanged {
                    node_id,
                    hostname,
                    status,
                    message,
                } => {
                    set(&obj, "variant", "MeshPeerStatusChanged".into());
                    set(&obj, "nodeId", node_id.clone().into());
                    set(&obj, "node_id", node_id.into());
                    set(&obj, "hostname", hostname.into());
                    set(&obj, "status", status.into());
                    set(&obj, "message", message.into());
                }
            }
        }
        MessageBody::AddonPermissionChangedEventBody(evt) => {
            set(&obj, "variant", "AddonPermissionChangedEvent".into());
            set(&obj, "addonId", evt.addon_id.clone().into());
            set(&obj, "addon_id", evt.addon_id.into());
            if let Some(st) = evt.subject_type {
                set(&obj, "subjectType", st.clone().into());
                set(&obj, "subject_type", st.into());
            }
            if let Some(sid) = evt.subject_id {
                set(&obj, "subjectId", (sid as f64).into());
                set(&obj, "subject_id", (sid as f64).into());
            }
            if let Some(pid) = evt.permission_id {
                set(&obj, "permissionId", pid.clone().into());
                set(&obj, "permission_id", pid.into());
            }
        }
        // ---- Addon lifecycle — request variants (echo pol dla kompletnosci) ----
        MessageBody::AddonToggleRequestBody(r) => {
            set(&obj, "variant", "AddonToggleRequest".into());
            set(&obj, "addonId", r.addon_id.into());
            set(&obj, "enabled", r.enabled.into());
        }
        MessageBody::AddonInstallRequestBody(r) => {
            set(&obj, "variant", "AddonInstallRequest".into());
            set(&obj, "filename", r.filename.into());
            set(&obj, "contentSize", (r.content.len() as f64).into());
        }
        MessageBody::AddonUninstallRequestBody(r) => {
            set(&obj, "variant", "AddonUninstallRequest".into());
            set(&obj, "addonId", r.addon_id.into());
        }
        MessageBody::AddonConfigGetRequestBody(r) => {
            set(&obj, "variant", "AddonConfigGetRequest".into());
            set(&obj, "addonId", r.addon_id.into());
        }
        MessageBody::AddonConfigSetRequestBody(r) => {
            set(&obj, "variant", "AddonConfigSetRequest".into());
            set(&obj, "addonId", r.addon_id.into());
            set(&obj, "valuesCount", (r.values.len() as f64).into());
        }
        MessageBody::AddonLogsRequestBody(r) => {
            set(&obj, "variant", "AddonLogsRequest".into());
            set(&obj, "addonId", r.addon_id.into());
            set(&obj, "limit", (r.limit as f64).into());
            set(&obj, "offset", (r.offset as f64).into());
        }
        MessageBody::AddonToolsRequestBody(r) => {
            set(&obj, "variant", "AddonToolsRequest".into());
            set(&obj, "addonId", r.addon_id.into());
        }
        MessageBody::AddonResourcesGetRequestBody(r) => {
            set(&obj, "variant", "AddonResourcesGetRequest".into());
            set(&obj, "addonId", r.addon_id.into());
        }
        MessageBody::AddonResourcesSetRequestBody(r) => {
            set(&obj, "variant", "AddonResourcesSetRequest".into());
            set(&obj, "addonId", r.addon_id.into());
        }
        MessageBody::AddonNetworkRulesGetRequestBody(r) => {
            set(&obj, "variant", "AddonNetworkRulesGetRequest".into());
            set(&obj, "addonId", r.addon_id.into());
        }
        MessageBody::AddonNetworkRulesSetRequestBody(r) => {
            set(&obj, "variant", "AddonNetworkRulesSetRequest".into());
            set(&obj, "addonId", r.addon_id.into());
        }
        MessageBody::AddonReloadRequestBody(r) => {
            set(&obj, "variant", "AddonReloadRequest".into());
            set(&obj, "addonId", r.addon_id.into());
        }
        // ---- Addon lifecycle — response variants (faktycznie dekodowane w GUI) ----
        MessageBody::AddonToggleResponseBody(r) => {
            set(&obj, "variant", "AddonToggleResponse".into());
            set(&obj, "ok", r.ok.into());
            set(&obj, "enabled", r.enabled.into());
            if let Some(m) = r.message {
                set(&obj, "message", m.into());
            }
        }
        MessageBody::AddonInstallResponseBody(r) => {
            set(&obj, "variant", "AddonInstallResponse".into());
            set(&obj, "ok", r.ok.into());
            if let Some(id) = r.addon_id {
                set(&obj, "addonId", id.into());
            }
            if let Some(v) = r.version {
                set(&obj, "version", v.into());
            }
            let warns = js_sys::Array::new();
            for w in r.warnings {
                warns.push(&w.into());
            }
            set(&obj, "warnings", warns.into());
            if let Some(e) = r.error {
                set(&obj, "error", e.into());
            }
        }
        MessageBody::AddonUninstallResponseBody(r) => {
            set(&obj, "variant", "AddonUninstallResponse".into());
            set(&obj, "ok", r.ok.into());
        }
        MessageBody::AddonConfigGetResponseBody(r) => {
            set(&obj, "variant", "AddonConfigGetResponse".into());
            let schema_arr = js_sys::Array::new();
            for f in r.schema {
                let fo = js_sys::Object::new();
                set(&fo, "id", f.id.into());
                set(&fo, "label", f.label.into());
                set(&fo, "type", f.field_type.into());
                set(&fo, "description", f.description.into());
                set(&fo, "defaultValue", f.default_value.into());
                let opts = js_sys::Array::new();
                for o in f.options {
                    opts.push(&o.into());
                }
                set(&fo, "options", opts.into());
                set(&fo, "required", f.required.into());
                set(&fo, "secret", f.secret.into());
                schema_arr.push(&fo.into());
            }
            set(&obj, "schema", schema_arr.into());
            let vals_arr = js_sys::Array::new();
            for (k, v) in r.values {
                let pair = js_sys::Array::new();
                pair.push(&k.into());
                pair.push(&v.into());
                vals_arr.push(&pair.into());
            }
            set(&obj, "values", vals_arr.into());
        }
        MessageBody::AddonConfigSetResponseBody(r) => {
            set(&obj, "variant", "AddonConfigSetResponse".into());
            set(&obj, "ok", r.ok.into());
        }
        MessageBody::AddonLogsResponseBody(r) => {
            set(&obj, "variant", "AddonLogsResponse".into());
            let arr = js_sys::Array::new();
            for e in r.entries {
                let eo = js_sys::Object::new();
                set(&eo, "id", (e.id as f64).into());
                set(&eo, "timestamp", e.timestamp.into());
                set(&eo, "level", e.level.into());
                set(&eo, "action", e.action.into());
                set(&eo, "message", e.message.into());
                if let Some(uid) = e.user_id {
                    set(&eo, "userId", (uid as f64).into());
                }
                if let Some(un) = e.user_name {
                    set(&eo, "userName", un.into());
                }
                set(&eo, "details", e.details.into());
                arr.push(&eo.into());
            }
            set(&obj, "entries", arr.into());
            set(&obj, "total", (r.total as f64).into());
        }
        MessageBody::AddonToolsResponseBody(r) => {
            set(&obj, "variant", "AddonToolsResponse".into());
            let arr = js_sys::Array::new();
            for t in r.tools {
                let to = js_sys::Object::new();
                set(&to, "name", t.name.into());
                set(&to, "description", t.description.into());
                set(&to, "returnType", t.return_type.into());
                let params = js_sys::Array::new();
                for p in t.parameters {
                    let po = js_sys::Object::new();
                    set(&po, "name", p.name.into());
                    set(&po, "type", p.param_type.into());
                    set(&po, "description", p.description.into());
                    set(&po, "required", p.required.into());
                    if let Some(d) = p.default_value {
                        set(&po, "defaultValue", d.into());
                    }
                    params.push(&po.into());
                }
                set(&to, "parameters", params.into());
                arr.push(&to.into());
            }
            set(&obj, "tools", arr.into());
        }
        MessageBody::AddonResourcesGetResponseBody(r) => {
            set(&obj, "variant", "AddonResourcesGetResponse".into());
            set(&obj, "maxInstances", (r.max_instances as f64).into());
            set(&obj, "cpuLimitPct", (r.cpu_limit_pct as f64).into());
            set(&obj, "ramMb", (r.ram_mb as f64).into());
            set(&obj, "storageMb", (r.storage_mb as f64).into());
            set(&obj, "httpRequestsPerMin", (r.http_requests_per_min as f64).into());
            set(&obj, "llmTokensPerMin", (r.llm_tokens_per_min as f64).into());
        }
        MessageBody::AddonResourcesSetResponseBody(r) => {
            set(&obj, "variant", "AddonResourcesSetResponse".into());
            set(&obj, "ok", r.ok.into());
        }
        MessageBody::AddonNetworkRulesGetResponseBody(r) => {
            set(&obj, "variant", "AddonNetworkRulesGetResponse".into());
            let allowed = js_sys::Array::new();
            for h in r.allowed_hosts {
                allowed.push(&h.into());
            }
            set(&obj, "allowedHosts", allowed.clone().into());
            set(&obj, "allowed_hosts", allowed.into());
            let blocked = js_sys::Array::new();
            for h in r.blocked_hosts {
                blocked.push(&h.into());
            }
            set(&obj, "blockedHosts", blocked.clone().into());
            set(&obj, "blocked_hosts", blocked.into());
            set(&obj, "mode", r.mode.into());
            let declared = js_sys::Array::new();
            for d in r.declared_rules {
                let item = js_sys::Object::new();
                set(&item, "host", d.host.into());
                match d.port {
                    Some(p) => set(&item, "port", (p as f64).into()),
                    None => set(&item, "port", JsValue::NULL),
                }
                set(&item, "mode", d.mode.into());
                set(&item, "status", d.status.into());
                declared.push(&item.into());
            }
            set(&obj, "declaredRules", declared.clone().into());
            set(&obj, "declared_rules", declared.into());
        }
        MessageBody::AddonNetworkRulesSetResponseBody(r) => {
            set(&obj, "variant", "AddonNetworkRulesSetResponse".into());
            set(&obj, "ok", r.ok.into());
        }
        MessageBody::AddonReloadResponseBody(r) => {
            set(&obj, "variant", "AddonReloadResponse".into());
            set(&obj, "ok", r.ok.into());
            if let Some(m) = r.message {
                set(&obj, "message", m.into());
            }
        }
        MessageBody::MeetingBody(p) => {
            meeting_payload_to_js(&obj, p);
        }
        MessageBody::VncTunnelBody(p) => {
            vnc_tunnel_payload_to_js(&obj, p);
        }
        MessageBody::BrowserCaptureBody(payload) => match payload {
            tentaflow_protocol::BrowserCapturePayload::Request(r) => {
                set(&obj, "variant", "BrowserCaptureRequest".into());
                set(&obj, "sessionId", (r.session_id as f64).into());
                set(&obj, "session_id", (r.session_id as f64).into());
                set(&obj, "kind", r.kind.into());
                set(&obj, "fullPage", r.full_page.into());
                set(&obj, "full_page", r.full_page.into());
            }
            tentaflow_protocol::BrowserCapturePayload::Response(r) => {
                set(&obj, "variant", "BrowserCaptureResponse".into());
                set(&obj, "status", r.status.into());
                set(&obj, "kind", r.kind.into());
                // Browser → JS: surowy PNG jako Uint8Array, DOM jako string.
                let png = js_sys::Uint8Array::from(r.png.as_slice());
                set(&obj, "png", png.into());
                set(&obj, "html", r.html.into());
                set(&obj, "error", r.error.into());
            }
        },
        MessageBody::MeetingLiveEventBody(event) => {
            set(&obj, "variant", "MeetingLiveEventBody".into());
            set(&obj, "meetingKey", event.meeting_key.clone().into());
            set(&obj, "timestampMs", (event.timestamp_ms as f64).into());
            let payload = js_sys::Object::new();
            meeting_event_payload_to_js(&payload, event.payload);
            set(&obj, "payload", payload.into());
        }
        MessageBody::NetworkBody(p) => {
            use tentaflow_protocol::NetworkPayload as NP;
            match p {
                NP::ReqInterfacesList => {
                    set(&obj, "variant", "NetworkInterfacesListRequest".into());
                }
                NP::ResInterfacesList { interfaces } => {
                    set(&obj, "variant", "NetworkInterfacesListResponse".into());
                    let arr = js_sys::Array::new();
                    for iface in interfaces.iter() {
                        arr.push(&network_interface_info_to_js(iface).into());
                    }
                    set(&obj, "interfaces", arr.into());
                }
                NP::ReqConfigGet => {
                    set(&obj, "variant", "NetworkConfigGetRequest".into());
                }
                NP::ResConfigGet(cfg) => {
                    set(&obj, "variant", "NetworkConfigGetResponse".into());
                    set(&obj, "config", network_config_to_js(&cfg).into());
                }
                NP::ReqConfigUpdate(cfg) => {
                    set(&obj, "variant", "NetworkConfigUpdateRequest".into());
                    set(&obj, "config", network_config_to_js(&cfg).into());
                }
                NP::ResConfigUpdate { restart_required } => {
                    set(&obj, "variant", "NetworkConfigUpdateResponse".into());
                    set(&obj, "restartRequired", restart_required.into());
                    set(&obj, "restart_required", restart_required.into());
                }
                NP::ReqRelayStatus => {
                    set(&obj, "variant", "NetworkRelayStatusRequest".into());
                }
                NP::ResRelayStatus(info) => {
                    set(&obj, "variant", "NetworkRelayStatusResponse".into());
                    set(&obj, "url", info.url.clone().into());
                    set(&obj, "reachable", info.reachable.into());
                    set(&obj, "rttMs", (info.rtt_ms as f64).into());
                    set(&obj, "rtt_ms", (info.rtt_ms as f64).into());
                    set(&obj, "lastCheckUnixSecs", (info.last_check_unix_secs as f64).into());
                    set(&obj, "last_check_unix_secs", (info.last_check_unix_secs as f64).into());
                    set(
                        &obj,
                        "lastSuccessUnixSecs",
                        (info.last_success_unix_secs as f64).into(),
                    );
                    set(
                        &obj,
                        "last_success_unix_secs",
                        (info.last_success_unix_secs as f64).into(),
                    );
                    set(&obj, "status", info.status.clone().into());
                    set(&obj, "bindAddrActual", info.bind_addr_actual.clone().into());
                    set(&obj, "bind_addr_actual", info.bind_addr_actual.clone().into());
                }
            }
        }
    }
    Ok(obj.into())
}

fn user_info_to_js(u: &tentaflow_protocol::UserInfo) -> js_sys::Object {
    let o = js_sys::Object::new();
    set(&o, "id", (u.id as f64).into());
    set(&o, "username", u.username.clone().into());
    set(&o, "displayName", u.display_name.clone().into());
    set(&o, "display_name", u.display_name.clone().into());
    set(&o, "email", u.email.clone().into());
    set(&o, "isActive", u.is_active.into());
    set(&o, "is_active", u.is_active.into());
    set(&o, "isAdmin", u.is_admin.into());
    set(&o, "is_admin", u.is_admin.into());
    set(&o, "role", u.role.clone().into());
    if let Some(p) = &u.sso_provider {
        set(&o, "ssoProvider", p.clone().into());
        set(&o, "sso_provider", p.clone().into());
    }
    if let Some(ts) = &u.last_login_at {
        set(&o, "lastLoginAt", ts.clone().into());
        set(&o, "last_login_at", ts.clone().into());
    }
    set(&o, "createdAt", u.created_at.clone().into());
    set(&o, "created_at", u.created_at.clone().into());
    let gs = js_sys::Array::new();
    for gid in &u.group_ids { gs.push(&(*gid as f64).into()); }
    set(&o, "groupIds", gs.into());
    o
}

fn deployment_summary_to_js(s: tentaflow_protocol::DeploymentSummary) -> js_sys::Object {
    let o = js_sys::Object::new();
    set(&o, "deployId", s.deploy_id.into());
    set(&o, "engineId", s.engine_id.into());
    set(&o, "deployMethod", s.deploy_method.into());
    set(&o, "nodeId", s.node_id.into());
    set(&o, "status", s.status.into());
    set(&o, "phase", s.phase.into());
    set(&o, "progressPct", s.progress_pct.into());
    set(&o, "imageTag", s.image_tag.into());
    set(&o, "containerName", s.container_name.into());
    set(&o, "startedAt", s.started_at.into());
    set(&o, "finishedAt", s.finished_at.into());
    set(&o, "errorMessage", s.error_message.into());
    set(&o, "logTail", s.log_tail.into());
    set(&o, "userId", (s.user_id as f64).into());
    o
}

fn deployment_payload_to_js(obj: &js_sys::Object, p: tentaflow_protocol::DeploymentPayload) {
    use tentaflow_protocol::DeploymentPayload as DP;
    match p {
        DP::ReqStart(req) => {
            set(obj, "variant", "ServiceManifestDeployRequest".into());
            set(obj, "engineId", req.engine_id.into());
            set(obj, "deployMethod", req.deploy_method.into());
            set(obj, "nodeId", req.node_id.into());
            set(obj, "configJson", req.config_json.into());
        }
        DP::ResStart(resp) => {
            set(obj, "variant", "ServiceManifestDeployResponse".into());
            set(obj, "status", resp.status.into());
            set(obj, "deployId", resp.deploy_id.into());
            set(obj, "engineId", resp.engine_id.into());
            set(obj, "deployMethod", resp.deploy_method.into());
            set(obj, "nodeId", resp.node_id.into());
            set(obj, "websocketUrl", resp.websocket_url.into());
        }
        DP::ReqStatus(req) => {
            set(obj, "variant", "DeploymentStatusRequest".into());
            set(obj, "deployId", req.deploy_id.into());
        }
        DP::ResStatus(resp) => {
            set(obj, "variant", "DeploymentStatusResponse".into());
            set(obj, "deployment", deployment_summary_to_js(resp.deployment).into());
        }
        DP::ReqList(req) => {
            set(obj, "variant", "DeploymentListRequest".into());
            set(obj, "engineId", req.engine_id.into());
            set(obj, "status", req.status.into());
            set(obj, "onlyMine", req.only_mine.into());
            set(obj, "limit", req.limit.into());
        }
        DP::ResList(resp) => {
            set(obj, "variant", "DeploymentListResponse".into());
            let arr = js_sys::Array::new();
            for d in resp.deployments {
                arr.push(&deployment_summary_to_js(d).into());
            }
            set(obj, "deployments", arr.into());
        }
        DP::ReqLogStream(req) => {
            set(obj, "variant", "DeploymentLogStreamRequest".into());
            set(obj, "deployId", req.deploy_id.into());
            set(obj, "replayTail", req.replay_tail.into());
        }
        DP::StreamChunk(c) => {
            set(obj, "variant", "DeploymentStreamChunk".into());
            set(obj, "deployId", c.deploy_id.into());
            set(obj, "kind", c.kind.into());
            set(obj, "line", c.line.into());
            set(obj, "phase", c.phase.into());
            set(obj, "progressPct", c.progress_pct.into());
            set(obj, "tsMs", (c.ts_ms as f64).into());
        }
        DP::StreamEnd(e) => {
            set(obj, "variant", "DeploymentStreamEnd".into());
            set(obj, "deployId", e.deploy_id.into());
            set(obj, "finalStatus", e.final_status.into());
            set(obj, "imageTag", e.image_tag.into());
            set(obj, "containerName", e.container_name.into());
            set(obj, "errorMessage", e.error_message.into());
            set(obj, "durationMs", (e.duration_ms as f64).into());
        }
        DP::ReqRedeploy(req) => {
            set(obj, "variant", "ServiceRedeployRequest".into());
            set(obj, "serviceId", (req.service_id as f64).into());
            set(obj, "forceIfActiveSessions", req.force_if_active_sessions.into());
        }
        DP::ResRedeploy(resp) => {
            set(obj, "variant", "ServiceRedeployResponse".into());
            set(obj, "status", resp.status.into());
            set(obj, "deployId", resp.deploy_id.into());
            set(obj, "newHash", resp.new_hash.into());
            set(obj, "error", resp.error.into());
            set(obj, "activeSessionCount", (resp.active_session_count as f64).into());
        }
    }
}

fn meeting_session_to_js(s: tentaflow_protocol::MeetingSessionDescriptor) -> js_sys::Object {
    let o = js_sys::Object::new();
    set(&o, "sessionId", (s.session_id as f64).into());
    set(&o, "meetingKey", s.meeting_key.into());
    set(&o, "meetingUrl", s.meeting_url.into());
    set(&o, "title", s.title.into());
    set(&o, "status", s.status.into());
    set(&o, "startedAt", s.started_at.into());
    set(&o, "lastActivityAt", s.last_activity_at.into());
    set(&o, "endedAt", s.ended_at.into());
    set(&o, "platform", s.platform.into());
    set(&o, "entryCount", (s.entry_count as f64).into());
    set(&o, "quicPort", s.quic_port.into());
    set(&o, "vncPort", s.vnc_port.into());
    set(&o, "novncPort", s.novnc_port.into());
    set(&o, "botEndpointId", s.bot_endpoint_id.into());
    set(&o, "containerName", s.container_name.into());
    set(&o, "ownerUserId", (s.owner_user_id as f64).into());
    // Lifecycle pola są kluczowe dla live view (chip LIVE/JOINING) i dla
    // onJoinClick który decyduje czy wracać do joining screen czy nawigować
    // wprost do live view po reload. Bez nich chip zawsze zostaje JOINING.
    set(&o, "lifecycleStage", s.lifecycle_stage.into());
    set(&o, "lifecycleDetails", s.lifecycle_details.into());
    // Backend models — empty string / -1 from the host means "not reported yet";
    // we surface JS null in that case so the live view can show a placeholder.
    let opt_str = |v: String| -> wasm_bindgen::JsValue {
        if v.is_empty() {
            wasm_bindgen::JsValue::NULL
        } else {
            v.into()
        }
    };
    let opt_num = |v: i64| -> wasm_bindgen::JsValue {
        if v < 0 {
            wasm_bindgen::JsValue::NULL
        } else {
            (v as f64).into()
        }
    };
    set(&o, "backendSttModel", opt_str(s.backend_stt_model));
    set(&o, "backendTtsModel", opt_str(s.backend_tts_model));
    set(
        &o,
        "backendSummarizationModel",
        opt_str(s.backend_summarization_model),
    );
    set(
        &o,
        "backendDiarizationModel",
        opt_str(s.backend_diarization_model),
    );
    set(
        &o,
        "backendStreamingLatencyMs",
        opt_num(s.backend_streaming_latency_ms),
    );
    set(
        &o,
        "backendEnrolledSpeakers",
        opt_num(s.backend_enrolled_speakers),
    );
    set(
        &o,
        "backendTotalParticipants",
        opt_num(s.backend_total_participants),
    );
    o
}

fn meeting_entry_to_js(e: tentaflow_protocol::MeetingTranscriptEntry) -> js_sys::Object {
    let o = js_sys::Object::new();
    set(&o, "id", (e.id as f64).into());
    set(&o, "sessionId", (e.session_id as f64).into());
    set(&o, "timestampMs", (e.timestamp_ms as f64).into());
    set(&o, "speaker", e.speaker.into());
    set(&o, "profileId", (e.profile_id as f64).into());
    set(&o, "confidence", (e.confidence as f64).into());
    set(&o, "isEnrolled", e.is_enrolled.into());
    set(&o, "text", e.text.into());
    set(&o, "model", e.model.into());
    o
}

fn vnc_tunnel_payload_to_js(obj: &js_sys::Object, p: tentaflow_protocol::VncTunnelPayload) {
    use tentaflow_protocol::VncTunnelPayload as VP;
    match p {
        VP::ReqOpen(r) => {
            set(obj, "variant", "VncTunnelOpenRequest".into());
            set(obj, "sessionId", (r.session_id as f64).into());
        }
        VP::ResOpen(r) => {
            set(obj, "variant", "VncTunnelOpenResponse".into());
            set(obj, "status", r.status.into());
            set(obj, "tunnelId", r.tunnel_id.into());
            set(obj, "error", r.error.into());
        }
        VP::Chunk(c) => {
            set(obj, "variant", "VncTunnelChunk".into());
            set(obj, "tunnelId", c.tunnel_id.into());
            set(obj, "bytes", js_sys::Uint8Array::from(c.bytes.as_slice()).into());
        }
        VP::ReqSend(r) => {
            set(obj, "variant", "VncTunnelSendRequest".into());
            set(obj, "tunnelId", r.tunnel_id.into());
            set(obj, "bytes", js_sys::Uint8Array::from(r.bytes.as_slice()).into());
        }
        VP::ResSend(r) => {
            set(obj, "variant", "VncTunnelSendResponse".into());
            set(obj, "ok", r.ok.into());
            set(obj, "error", r.error.into());
        }
        VP::ReqClose(r) => {
            set(obj, "variant", "VncTunnelCloseRequest".into());
            set(obj, "tunnelId", r.tunnel_id.into());
        }
        VP::ResClose(r) => {
            set(obj, "variant", "VncTunnelCloseResponse".into());
            set(obj, "ok", r.ok.into());
        }
        VP::StreamEnd(e) => {
            set(obj, "variant", "VncTunnelStreamEnd".into());
            set(obj, "tunnelId", e.tunnel_id.into());
            set(obj, "reason", e.reason.into());
        }
    }
}

fn meeting_payload_to_js(obj: &js_sys::Object, p: tentaflow_protocol::MeetingPayload) {
    use tentaflow_protocol::MeetingPayload as MP;
    match p {
        MP::ReqSessionStart(_) => set(obj, "variant", "MeetingSessionStartRequest".into()),
        MP::ResSessionStart(r) => {
            set(obj, "variant", "MeetingSessionStartResponse".into());
            set(obj, "session", meeting_session_to_js(r.session).into());
        }
        MP::ReqSessionLeave(_) => set(obj, "variant", "MeetingSessionLeaveRequest".into()),
        MP::ResSessionLeave(r) => {
            set(obj, "variant", "MeetingSessionLeaveResponse".into());
            set(obj, "ok", r.ok.into());
        }
        MP::ReqSessionList(_) => set(obj, "variant", "MeetingSessionListRequest".into()),
        MP::ResSessionList(r) => {
            set(obj, "variant", "MeetingSessionListResponse".into());
            let arr = js_sys::Array::new();
            for s in r.sessions {
                arr.push(&meeting_session_to_js(s).into());
            }
            set(obj, "sessions", arr.into());
        }
        MP::ReqSessionDetail(_) => set(obj, "variant", "MeetingSessionDetailRequest".into()),
        MP::ResSessionDetail(r) => {
            set(obj, "variant", "MeetingSessionDetailResponse".into());
            set(obj, "session", meeting_session_to_js(r.session).into());
            let arr = js_sys::Array::new();
            for e in r.transcripts {
                arr.push(&meeting_entry_to_js(e).into());
            }
            set(obj, "transcripts", arr.into());
        }
        MP::ReqTranscriptsList(_) => set(obj, "variant", "MeetingTranscriptsListRequest".into()),
        MP::ResTranscriptsList(r) => {
            set(obj, "variant", "MeetingTranscriptsListResponse".into());
            let arr = js_sys::Array::new();
            for e in r.entries {
                arr.push(&meeting_entry_to_js(e).into());
            }
            set(obj, "entries", arr.into());
        }
        MP::ReqActiveSession(_) => set(obj, "variant", "MeetingActiveSessionRequest".into()),
        MP::ResActiveSession(r) => {
            set(obj, "variant", "MeetingActiveSessionResponse".into());
            set(obj, "hasActive", r.has_active.into());
            set(obj, "session", meeting_session_to_js(r.session).into());
        }
        MP::ReqSettingsGet(_) => set(obj, "variant", "MeetingSettingsGetRequest".into()),
        MP::ResSettingsGet(r) => {
            set(obj, "variant", "MeetingSettingsGetResponse".into());
            let arr = js_sys::Array::new();
            for kv in r.settings {
                let o = js_sys::Object::new();
                set(&o, "key", kv.key.into());
                set(&o, "value", kv.value.into());
                arr.push(&o.into());
            }
            set(obj, "settings", arr.into());
        }
        MP::ReqSettingsUpdate(_) => set(obj, "variant", "MeetingSettingsUpdateRequest".into()),
        MP::ResSettingsUpdate(r) => {
            set(obj, "variant", "MeetingSettingsUpdateResponse".into());
            set(obj, "ok", r.ok.into());
        }
        MP::ReqSummariesList(_) => set(obj, "variant", "MeetingSummariesListRequest".into()),
        MP::ResSummariesList(r) => {
            set(obj, "variant", "MeetingSummariesListResponse".into());
            let arr = js_sys::Array::new();
            for s in r.items {
                arr.push(&meeting_summary_to_js(s).into());
            }
            set(obj, "items", arr.into());
        }
        MP::ReqActionItemsList(_) => set(obj, "variant", "MeetingActionItemsListRequest".into()),
        MP::ResActionItemsList(r) => {
            set(obj, "variant", "MeetingActionItemsListResponse".into());
            let arr = js_sys::Array::new();
            for a in r.items {
                arr.push(&meeting_action_item_to_js(a).into());
            }
            set(obj, "items", arr.into());
        }
        MP::ReqActionItemStatusUpdate(_) => {
            set(obj, "variant", "MeetingActionItemStatusUpdateRequest".into())
        }
        MP::ResActionItemStatusUpdate(r) => {
            set(obj, "variant", "MeetingActionItemStatusUpdateResponse".into());
            set(obj, "success", r.success.into());
        }
        MP::ReqTranscriptExport(_) => {
            set(obj, "variant", "MeetingTranscriptExportRequest".into())
        }
        MP::ResTranscriptExport(r) => {
            set(obj, "variant", "MeetingTranscriptExportResponse".into());
            set(obj, "content", r.content.into());
        }
    }
}

fn meeting_summary_to_js(s: tentaflow_protocol::MeetingSummaryItem) -> js_sys::Object {
    let o = js_sys::Object::new();
    set(&o, "id", (s.id as f64).into());
    set(&o, "createdAt", s.created_at.into());
    set(&o, "decisionsText", s.decisions_text.into());
    set(&o, "summaryText", s.summary_text.into());
    set(&o, "model", s.model.into());
    o
}

fn meeting_action_item_to_js(a: tentaflow_protocol::MeetingActionItemItem) -> js_sys::Object {
    let o = js_sys::Object::new();
    set(&o, "id", (a.id as f64).into());
    set(&o, "owner", a.owner.into());
    set(&o, "task", a.task.into());
    if let Some(d) = a.deadline {
        set(&o, "deadline", d.into());
    }
    set(&o, "status", a.status.into());
    set(&o, "createdAt", a.created_at.into());
    set(&o, "updatedAt", a.updated_at.into());
    o
}

/// Tlumaczy `MeetingEventPayload` na JS object. Pole `type` zawiera nazwe
/// wariantu ("SummaryUpdate" itd.), `data` zawiera splaszczone pola danych.
fn meeting_event_payload_to_js(
    obj: &js_sys::Object,
    p: tentaflow_protocol::MeetingEventPayload,
) {
    use tentaflow_protocol::MeetingEventPayload as EP;
    let data = js_sys::Object::new();
    match p {
        EP::SummaryUpdate {
            decisions_text,
            summary_text,
            model,
        } => {
            set(obj, "type", "SummaryUpdate".into());
            set(&data, "decisionsText", decisions_text.into());
            set(&data, "summaryText", summary_text.into());
            set(&data, "model", model.into());
        }
        EP::ActionItemsUpdate { items } => {
            set(obj, "type", "ActionItemsUpdate".into());
            let arr = js_sys::Array::new();
            for it in items {
                let io = js_sys::Object::new();
                set(&io, "owner", it.owner.into());
                set(&io, "task", it.task.into());
                if let Some(d) = it.deadline {
                    set(&io, "deadline", d.into());
                }
                arr.push(&io.into());
            }
            set(&data, "items", arr.into());
        }
        EP::TranscriptEntry {
            speaker_id,
            speaker_name,
            is_enrolled,
            speaker_confidence,
            text,
            language,
            resolved_stt_model,
            latency_ms,
        } => {
            set(obj, "type", "TranscriptEntry".into());
            set(&data, "speakerId", speaker_id.into());
            if let Some(n) = speaker_name {
                set(&data, "speakerName", n.into());
            }
            set(&data, "isEnrolled", is_enrolled.into());
            if let Some(c) = speaker_confidence {
                set(&data, "speakerConfidence", (c as f64).into());
            }
            set(&data, "text", text.into());
            if let Some(l) = language {
                set(&data, "language", l.into());
            }
            set(&data, "resolvedSttModel", resolved_stt_model.into());
            set(&data, "latencyMs", (latency_ms as f64).into());
        }
        EP::ParticipantUpdate {
            speaker_id,
            speaker_name,
            status,
            last_spoken_ago_sec,
        } => {
            set(obj, "type", "ParticipantUpdate".into());
            set(&data, "speakerId", speaker_id.into());
            if let Some(n) = speaker_name {
                set(&data, "speakerName", n.into());
            }
            set(&data, "status", status.into());
            if let Some(s) = last_spoken_ago_sec {
                set(&data, "lastSpokenAgoSec", (s as f64).into());
            }
        }
        EP::BackendUpdate {
            stt_model,
            tts_model,
            summarization_model,
            diarization_model,
            streaming_latency_ms,
            enrolled_speakers,
            total_participants,
        } => {
            set(obj, "type", "BackendUpdate".into());
            set(&data, "sttModel", stt_model.into());
            set(&data, "ttsModel", tts_model.into());
            set(&data, "summarizationModel", summarization_model.into());
            set(&data, "diarizationModel", diarization_model.into());
            if let Some(v) = streaming_latency_ms {
                set(&data, "streamingLatencyMs", (v as f64).into());
            }
            if let Some(v) = enrolled_speakers {
                set(&data, "enrolledSpeakers", (v as f64).into());
            }
            if let Some(v) = total_participants {
                set(&data, "totalParticipants", (v as f64).into());
            }
        }
        EP::LifecycleUpdate { stage, details } => {
            set(obj, "type", "LifecycleUpdate".into());
            set(&data, "stage", stage.into());
            if let Some(d) = details {
                set(&data, "details", d.into());
            }
        }
    }
    set(obj, "data", data.into());
}

fn flow_node_template_to_js(t: tentaflow_protocol::message_body::FlowNodeTemplate) -> js_sys::Object {
    let obj = js_sys::Object::new();
    // Emitujemy rownoczesnie camelCase (nowy kod) i snake_case (istniejaca paleta).
    set(&obj, "id", (t.id as f64).into());
    set(&obj, "nodeType", t.node_type.clone().into());
    set(&obj, "node_type", t.node_type.into());
    set(&obj, "category", t.category.into());
    set(&obj, "label", t.label.into());
    if let Some(d) = t.description {
        set(&obj, "description", d.into());
    }
    set(&obj, "defaultConfig", t.default_config.clone().into());
    set(&obj, "default_config", t.default_config.into());
    if let Some(i) = t.icon {
        set(&obj, "icon", i.into());
    }
    let input_ports = js_sys::Array::new();
    for p in &t.input_ports {
        input_ports.push(&JsValue::from_str(p));
    }
    set(&obj, "inputPorts", input_ports.clone().into());
    set(&obj, "input_ports", input_ports.into());
    let output_ports = js_sys::Array::new();
    for p in &t.output_ports {
        output_ports.push(&JsValue::from_str(p));
    }
    set(&obj, "outputPorts", output_ports.clone().into());
    set(&obj, "output_ports", output_ports.into());
    obj
}

fn flow_version_summary_to_js(
    v: tentaflow_protocol::message_body::FlowVersionSummary,
) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "id", v.id.into());
    set(&obj, "flowId", v.flow_id.clone().into());
    set(&obj, "flow_id", v.flow_id.into());
    set(&obj, "versionNum", (v.version_num as f64).into());
    set(&obj, "version_num", (v.version_num as f64).into());
    set(&obj, "name", v.name.into());
    if let Some(d) = v.description {
        set(&obj, "description", d.into());
    }
    if let Some(s) = v.status {
        set(&obj, "status", s.into());
    }
    set(&obj, "createdAtEpoch", v.created_at_epoch.into());
    set(&obj, "created_at_epoch", v.created_at_epoch.into());
    if let Some(cb) = v.created_by {
        set(&obj, "createdBy", cb.clone().into());
        set(&obj, "created_by", cb.into());
    }
    obj
}

fn flow_version_full_to_js(
    v: tentaflow_protocol::message_body::FlowVersionFull,
) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "id", v.id.into());
    set(&obj, "flowId", v.flow_id.clone().into());
    set(&obj, "flow_id", v.flow_id.into());
    set(&obj, "versionNum", (v.version_num as f64).into());
    set(&obj, "version_num", (v.version_num as f64).into());
    set(&obj, "name", v.name.into());
    if let Some(d) = v.description {
        set(&obj, "description", d.into());
    }
    if let Some(s) = v.status {
        set(&obj, "status", s.into());
    }
    set(&obj, "flowJson", v.flow_json.clone().into());
    set(&obj, "flow_json", v.flow_json.into());
    set(&obj, "createdAtEpoch", v.created_at_epoch.into());
    set(&obj, "created_at_epoch", v.created_at_epoch.into());
    if let Some(cb) = v.created_by {
        set(&obj, "createdBy", cb.clone().into());
        set(&obj, "created_by", cb.into());
    }
    obj
}

fn model_alias_entry_to_js(a: tentaflow_protocol::ModelAliasEntry) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "id", (a.id as f64).into());
    set(&obj, "alias", a.alias.into());
    set(&obj, "targetModel", a.target_model.clone().into());
    set(&obj, "target_model", a.target_model.into());
    set(&obj, "isActive", a.is_active.into());
    set(&obj, "is_active", a.is_active.into());
    if let Some(f) = a.fallback_targets {
        set(&obj, "fallbackTargets", f.clone().into());
        set(&obj, "fallback_targets", f.into());
    }
    if let Some(s) = a.strategy { set(&obj, "strategy", s.into()); }
    obj
}

fn mesh_node_info_to_js(n: tentaflow_protocol::MeshNodeInfo) -> js_sys::Object {
    let obj = js_sys::Object::new();
    // Emitujemy zarowno camelCase (dla nowego kodu) jak i snake_case aliasy
    // (dla istniejacego kodu mesh.js / mesh-detail.js ktory czyta REST-shape).
    set(&obj, "nodeId", n.node_id.clone().into());
    set(&obj, "node_id", n.node_id.into());
    set(&obj, "hostname", n.hostname.into());
    if let Some(ref ip) = n.ip { set(&obj, "ip", ip.clone().into()); }
    set(&obj, "status", n.status.into());
    set(&obj, "source", n.source.clone().into());
    set(&obj, "trust", n.source.into());
    set(&obj, "isLocal", n.is_local.into());
    set(&obj, "is_local", n.is_local.into());
    if let Some(v) = n.uptime_secs {
        set(&obj, "uptimeSecs", (v as f64).into());
        set(&obj, "uptime_secs", (v as f64).into());
    }
    let ifs = js_sys::Array::new();
    let mut total_rx: u64 = 0;
    let mut total_tx: u64 = 0;
    for i in n.network_interfaces {
        let item = js_sys::Object::new();
        set(&item, "name", i.name.into());
        set(&item, "linkUp", i.link_up.into());
        set(&item, "link_up", i.link_up.into());
        if let Some(v) = i.speed_mbps {
            set(&item, "speedMbps", v.into());
            set(&item, "speed_mbps", v.into());
        }
        if let Some(v) = i.ipv4_address {
            set(&item, "ipv4Address", v.clone().into());
            set(&item, "ipv4_address", v.into());
        }
        if let Some(v) = i.interface_type {
            set(&item, "interfaceType", v.clone().into());
            set(&item, "interface_type", v.into());
        }
        if let Some(v) = i.rdma_available {
            set(&item, "rdmaAvailable", v.into());
            set(&item, "rdma_available", v.into());
        }
        if let Some(v) = i.roce_available {
            set(&item, "roceAvailable", v.into());
            set(&item, "roce_available", v.into());
        }
        if let Some(v) = i.numa_node {
            set(&item, "numaNode", v.into());
            set(&item, "numa_node", v.into());
        }
        if let Some(v) = i.rx_bytes_per_sec {
            set(&item, "rxBytesPerSec", (v as f64).into());
            set(&item, "rx_bytes_per_sec", (v as f64).into());
            total_rx += v;
        }
        if let Some(v) = i.tx_bytes_per_sec {
            set(&item, "txBytesPerSec", (v as f64).into());
            set(&item, "tx_bytes_per_sec", (v as f64).into());
            total_tx += v;
        }
        ifs.push(&item.into());
    }
    set(&obj, "networkInterfaces", ifs.clone().into());
    set(&obj, "network_interfaces", ifs.into());
    set(&obj, "network_rx_bytes", (total_rx as f64).into());
    set(&obj, "network_tx_bytes", (total_tx as f64).into());
    if let Some(v) = n.cpu_count {
        set(&obj, "cpuCount", v.into());
        set(&obj, "cpu_count", v.into());
    }
    if let Some(v) = n.cpu_usage_percent {
        set(&obj, "cpuUsagePercent", (v as f64).into());
        set(&obj, "cpu_usage_percent", (v as f64).into());
        set(&obj, "cpu_usage", (v as f64).into());
    }
    if let Some(v) = n.ram_total_mb {
        set(&obj, "ramTotalMb", (v as f64).into());
        set(&obj, "ram_total_mb", (v as f64).into());
    }
    if let Some(v) = n.ram_used_mb {
        set(&obj, "ramUsedMb", (v as f64).into());
        set(&obj, "ram_used_mb", (v as f64).into());
    }
    if let Some(v) = n.vram_total_mb {
        set(&obj, "vramTotalMb", (v as f64).into());
        set(&obj, "vram_total_mb", (v as f64).into());
    }
    if let Some(v) = n.vram_used_mb {
        set(&obj, "vramUsedMb", (v as f64).into());
        set(&obj, "vram_used_mb", (v as f64).into());
    }
    if let Some(v) = n.gpu_load_percent {
        set(&obj, "gpuLoadPercent", (v as f64).into());
        set(&obj, "gpu_load_percent", (v as f64).into());
    }
    if let Some(connection) = &n.connection {
        let connection_obj = js_sys::Object::new();
        set(&connection_obj, "transport", connection.transport.clone().into());
        if let Some(scope) = &connection.scope {
            set(&connection_obj, "scope", scope.clone().into());
        }
        if let Some(address) = &connection.address {
            set(&connection_obj, "address", address.clone().into());
        }
        if let Some(relay_url) = &connection.relay_url {
            set(&connection_obj, "relayUrl", relay_url.clone().into());
            set(&connection_obj, "relay_url", relay_url.clone().into());
        }
        let paths = js_sys::Array::new();
        for path in &connection.paths {
            let path_obj = js_sys::Object::new();
            set(&path_obj, "transport", path.transport.clone().into());
            set(&path_obj, "address", path.address.clone().into());
            set(&path_obj, "selected", path.selected.into());
            set(&path_obj, "closed", path.closed.into());
            paths.push(&path_obj.into());
        }
        set(&connection_obj, "paths", paths.into());
        set(&obj, "connection", connection_obj.into());
    }
    // Per-GPU list — emitted in both camelCase and snake_case variants so
    // callers can render individual cards and per-GPU deploy targeting.
    let gpu_arr = js_sys::Array::new();
    for g in &n.gpus {
        let item = js_sys::Object::new();
        set(&item, "vendor", g.vendor.clone().into());
        set(&item, "name", g.name.clone().into());
        set(&item, "vramTotalMb", (g.vram_total_mb as f64).into());
        set(&item, "vram_total_mb", (g.vram_total_mb as f64).into());
        if let Some(v) = g.vram_used_mb {
            set(&item, "vramUsedMb", (v as f64).into());
            set(&item, "vram_used_mb", (v as f64).into());
        }
        if let Some(v) = g.utilization_percent {
            set(&item, "utilizationPercent", (v as f64).into());
            set(&item, "usage_percent", (v as f64).into());
        }
        if let Some(v) = g.temperature_c {
            set(&item, "temperatureC", (v as f64).into());
            set(&item, "temperature_c", (v as f64).into());
        }
        if let Some(v) = g.power_draw_w {
            set(&item, "powerDrawW", (v as f64).into());
            set(&item, "power_draw_w", (v as f64).into());
        }
        if let Some(ref v) = g.driver_version {
            set(&item, "driverVersion", v.clone().into());
            set(&item, "driver_version", v.clone().into());
        }
        if let Some(ref v) = g.cuda_version {
            set(&item, "cudaVersion", v.clone().into());
            set(&item, "cuda_version", v.clone().into());
        }
        gpu_arr.push(&item.into());
    }
    set(&obj, "gpus", gpu_arr.clone().into());
    set(&obj, "gpu_count", (gpu_arr.length() as u32).into());
    let models = js_sys::Array::new();
    for m in n.models {
        let item = js_sys::Object::new();
        set(&item, "alias", m.alias.into());
        if let Some(v) = m.kind { set(&item, "kind", v.into()); }
        if let Some(v) = m.backend { set(&item, "backend", v.into()); }
        if let Some(v) = m.size_mb {
            set(&item, "sizeMb", (v as f64).into());
            set(&item, "size_mb", (v as f64).into());
        }
        set(&item, "loaded", m.loaded.into());
        models.push(&item.into());
    }
    set(&obj, "models", models.into());
    let containers = js_sys::Array::new();
    let mut containers_running: u32 = 0;
    for c in n.containers {
        let item = js_sys::Object::new();
        set(&item, "name", c.name.into());
        set(&item, "image", c.image.into());
        let status = c.status.clone();
        set(&item, "status", c.status.into());
        if status.contains("running") || status.contains("Up") {
            containers_running += 1;
        }
        if let Some(v) = c.cpu_percent {
            set(&item, "cpuPercent", (v as f64).into());
            set(&item, "cpu_percent", (v as f64).into());
        }
        if let Some(v) = c.memory_mb {
            set(&item, "memoryMb", (v as f64).into());
            set(&item, "memory_mb", (v as f64).into());
        }
        if let Some(v) = c.memory_limit_mb {
            set(&item, "memoryLimitMb", (v as f64).into());
            set(&item, "memory_limit_mb", (v as f64).into());
        }
        containers.push(&item.into());
    }
    let containers_total = containers.length() as u32;
    set(&obj, "containers", containers.into());
    set(&obj, "containers_running", containers_running.into());
    set(&obj, "containers_total", containers_total.into());
    if let Some(v) = n.last_seen_epoch {
        set(&obj, "lastSeenEpoch", (v as f64).into());
        set(&obj, "last_seen_epoch", (v as f64).into());
    }
    if let Some(r) = n.route {
        let route = js_sys::Object::new();
        set(&route, "hops", r.hops.into());
        set(&route, "direct", r.direct.into());
        if let Some(v) = r.next_hop {
            set(&route, "nextHop", v.clone().into());
            set(&route, "next_hop", v.into());
        }
        set(&obj, "route", route.into());
    }
    set(&obj, "platform", n.platform.clone().into());
    obj
}

fn cluster_info_to_js(c: tentaflow_protocol::ClusterInfo) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "id", c.id.into());
    set(&obj, "name", c.name.into());
    if let Some(d) = c.description { set(&obj, "description", d.into()); }
    set(&obj, "strategy", c.strategy.into());
    set(&obj, "status", c.status.into());
    set(&obj, "membersCount", c.members_count.into());
    set(&obj, "membersOnline", c.members_online.into());
    set(&obj, "createdAt", (c.created_at as f64).into());
    set(&obj, "updatedAt", (c.updated_at as f64).into());
    set(&obj, "failoverEnabled", c.failover_enabled.into());
    if let Some(t) = c.failover_target { set(&obj, "failoverTarget", t.into()); }
    set(&obj, "healthCheckIntervalMs", c.health_check_interval_ms.into());
    set(&obj, "timeoutMs", c.timeout_ms.into());
    obj
}

// =============================================================================
// Helpers: struktury pomocnicze addon permissions + OAuth
// =============================================================================

/// Konwertuje `AddonPermissionDecl` na JS object z polami w obu nazewnictwach.
fn addon_permission_decl_to_js(
    p: tentaflow_protocol::message_body::AddonPermissionDecl,
) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "permissionId", p.permission_id.clone().into());
    set(&obj, "permission_id", p.permission_id.into());
    set(&obj, "displayName", p.display_name.clone().into());
    set(&obj, "display_name", p.display_name.into());
    set(&obj, "description", p.description.into());
    set(&obj, "risk", p.risk.into());
    set(&obj, "sortOrder", p.sort_order.into());
    set(&obj, "sort_order", p.sort_order.into());
    obj
}

/// Konwertuje `AddonPermissionRow` (explicit allow/deny/inherit per subject).
fn addon_permission_row_to_js(
    r: tentaflow_protocol::message_body::AddonPermissionRow,
) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "addonId", r.addon_id.clone().into());
    set(&obj, "addon_id", r.addon_id.into());
    set(&obj, "subjectType", r.subject_type.clone().into());
    set(&obj, "subject_type", r.subject_type.into());
    set(&obj, "subjectId", (r.subject_id as f64).into());
    set(&obj, "subject_id", (r.subject_id as f64).into());
    set(&obj, "permissionId", r.permission_id.clone().into());
    set(&obj, "permission_id", r.permission_id.into());
    set(&obj, "grantMode", r.grant_mode.clone().into());
    set(&obj, "grant_mode", r.grant_mode.into());
    set(&obj, "updatedAtEpoch", (r.updated_at_epoch as f64).into());
    set(&obj, "updated_at_epoch", (r.updated_at_epoch as f64).into());
    obj
}

/// Konwertuje `AddonPermissionDefault` (fallback dla addona).
fn addon_permission_default_to_js(
    d: tentaflow_protocol::message_body::AddonPermissionDefault,
) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "addonId", d.addon_id.clone().into());
    set(&obj, "addon_id", d.addon_id.into());
    set(&obj, "permissionId", d.permission_id.clone().into());
    set(&obj, "permission_id", d.permission_id.into());
    set(&obj, "grantMode", d.grant_mode.clone().into());
    set(&obj, "grant_mode", d.grant_mode.into());
    set(&obj, "updatedAtEpoch", (d.updated_at_epoch as f64).into());
    set(&obj, "updated_at_epoch", (d.updated_at_epoch as f64).into());
    obj
}

/// Konwertuje `AddonOAuthProviderDecl` (deklaracja providera w manifescie).
fn addon_oauth_provider_decl_to_js(
    p: tentaflow_protocol::message_body::AddonOAuthProviderDecl,
) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "addonId", p.addon_id.clone().into());
    set(&obj, "addon_id", p.addon_id.into());
    set(&obj, "providerId", p.provider_id.clone().into());
    set(&obj, "provider_id", p.provider_id.into());
    set(&obj, "displayName", p.display_name.clone().into());
    set(&obj, "display_name", p.display_name.into());
    set(&obj, "authorizeUrl", p.authorize_url.clone().into());
    set(&obj, "authorize_url", p.authorize_url.into());
    set(&obj, "tokenUrl", p.token_url.clone().into());
    set(&obj, "token_url", p.token_url.into());
    if let Some(r) = p.revoke_url {
        set(&obj, "revokeUrl", r.clone().into());
        set(&obj, "revoke_url", r.into());
    }
    let scopes = js_sys::Array::new();
    for s in p.scopes {
        scopes.push(&JsValue::from_str(&s));
    }
    set(&obj, "scopes", scopes.into());
    set(&obj, "mode", p.mode.into());
    set(&obj, "pkce", p.pkce.into());
    obj
}

/// Konwertuje `AddonOAuthConfigRow` (konfig po stronie admina — zero secretow).
fn addon_oauth_config_row_to_js(
    c: tentaflow_protocol::message_body::AddonOAuthConfigRow,
) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "addonId", c.addon_id.clone().into());
    set(&obj, "addon_id", c.addon_id.into());
    set(&obj, "providerId", c.provider_id.clone().into());
    set(&obj, "provider_id", c.provider_id.into());
    set(&obj, "clientId", c.client_id.clone().into());
    set(&obj, "client_id", c.client_id.into());
    set(&obj, "clientSecretSet", c.client_secret_set.into());
    set(&obj, "client_secret_set", c.client_secret_set.into());
    set(&obj, "redirectUri", c.redirect_uri.clone().into());
    set(&obj, "redirect_uri", c.redirect_uri.into());
    set(&obj, "enabled", c.enabled.into());
    set(&obj, "updatedAtEpoch", (c.updated_at_epoch as f64).into());
    set(&obj, "updated_at_epoch", (c.updated_at_epoch as f64).into());
    set(&obj, "oauthMode", c.oauth_mode.clone().into());
    set(&obj, "oauth_mode", c.oauth_mode.into());
    set(
        &obj,
        "linkedAccountsCount",
        (c.linked_accounts_count as f64).into(),
    );
    set(
        &obj,
        "linked_accounts_count",
        (c.linked_accounts_count as f64).into(),
    );
    if let Some(email) = c.shared_account_email {
        set(&obj, "sharedAccountEmail", email.clone().into());
        set(&obj, "shared_account_email", email.into());
    }
    obj
}

/// Konwertuje `UserOAuthAccountRow` (metadata konta — tokeny NIE serializowane).
fn user_oauth_account_row_to_js(
    a: tentaflow_protocol::message_body::UserOAuthAccountRow,
) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "id", (a.id as f64).into());
    if let Some(uid) = a.user_id {
        set(&obj, "userId", (uid as f64).into());
        set(&obj, "user_id", (uid as f64).into());
    }
    set(&obj, "addonId", a.addon_id.clone().into());
    set(&obj, "addon_id", a.addon_id.into());
    set(&obj, "providerId", a.provider_id.clone().into());
    set(&obj, "provider_id", a.provider_id.into());
    set(&obj, "externalAccountId", a.external_account_id.clone().into());
    set(&obj, "external_account_id", a.external_account_id.into());
    set(&obj, "displayName", a.display_name.clone().into());
    set(&obj, "display_name", a.display_name.into());
    set(&obj, "tokenType", a.token_type.clone().into());
    set(&obj, "token_type", a.token_type.into());
    let scopes = js_sys::Array::new();
    for s in a.scopes {
        scopes.push(&JsValue::from_str(&s));
    }
    set(&obj, "scopes", scopes.into());
    if let Some(v) = a.expires_at_epoch {
        set(&obj, "expiresAtEpoch", (v as f64).into());
        set(&obj, "expires_at_epoch", (v as f64).into());
    }
    set(&obj, "createdAtEpoch", (a.created_at_epoch as f64).into());
    set(&obj, "created_at_epoch", (a.created_at_epoch as f64).into());
    if let Some(v) = a.last_used_at_epoch {
        set(&obj, "lastUsedAtEpoch", (v as f64).into());
        set(&obj, "last_used_at_epoch", (v as f64).into());
    }
    set(&obj, "revoked", a.revoked.into());
    obj
}

/// Konwertuje `MyOAuthEntry` (wiersz widoku "Moje polaczone konta").
fn my_oauth_entry_to_js(
    e: tentaflow_protocol::message_body::MyOAuthEntry,
) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "addonId", e.addon_id.clone().into());
    set(&obj, "addon_id", e.addon_id.into());
    set(&obj, "addonName", e.addon_name.clone().into());
    set(&obj, "addon_name", e.addon_name.into());
    if let Some(icon) = e.addon_icon {
        set(&obj, "addonIcon", icon.clone().into());
        set(&obj, "addon_icon", icon.into());
    } else {
        set(&obj, "addonIcon", JsValue::NULL);
        set(&obj, "addon_icon", JsValue::NULL);
    }
    set(&obj, "addonDescription", e.addon_description.clone().into());
    set(&obj, "addon_description", e.addon_description.into());
    set(&obj, "addonVersion", e.addon_version.clone().into());
    set(&obj, "addon_version", e.addon_version.into());
    set(&obj, "providerId", e.provider_id.clone().into());
    set(&obj, "provider_id", e.provider_id.into());
    set(&obj, "providerDisplayName", e.provider_display_name.clone().into());
    set(&obj, "provider_display_name", e.provider_display_name.into());
    set(&obj, "status", e.status.into());
    if let Some(aid) = e.account_id {
        set(&obj, "accountId", (aid as f64).into());
        set(&obj, "account_id", (aid as f64).into());
    } else {
        set(&obj, "accountId", JsValue::NULL);
        set(&obj, "account_id", JsValue::NULL);
    }
    set(&obj, "accountEmail", e.account_email.clone().into());
    set(&obj, "account_email", e.account_email.into());
    set(&obj, "accountDisplayName", e.account_display_name.clone().into());
    set(&obj, "account_display_name", e.account_display_name.into());
    let scopes = js_sys::Array::new();
    for s in e.scopes {
        scopes.push(&JsValue::from_str(&s));
    }
    set(&obj, "scopes", scopes.into());
    set(&obj, "connectedAtEpoch", (e.connected_at_epoch as f64).into());
    set(&obj, "connected_at_epoch", (e.connected_at_epoch as f64).into());
    set(&obj, "lastUsedAtEpoch", (e.last_used_at_epoch as f64).into());
    set(&obj, "last_used_at_epoch", (e.last_used_at_epoch as f64).into());
    set(&obj, "expiresAtEpoch", (e.expires_at_epoch as f64).into());
    set(&obj, "expires_at_epoch", (e.expires_at_epoch as f64).into());
    obj
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
// Addon lifecycle (toggle/install/uninstall/config/logs/tools/resources/network/reload)
// =============================================================================

#[wasm_bindgen(js_name = encodeAddonToggleRequest)]
pub fn encode_addon_toggle_request(addon_id: String, enabled: bool) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonToggleRequestBody(AddonToggleRequest {
        addon_id,
        enabled,
    }))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAddonInstallRequest)]
pub fn encode_addon_install_request(
    filename: String,
    content: Vec<u8>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonInstallRequestBody(AddonInstallRequest {
        filename,
        content,
    }))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAddonUninstallRequest)]
pub fn encode_addon_uninstall_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonUninstallRequestBody(
        AddonUninstallRequest { addon_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAddonConfigGetRequest)]
pub fn encode_addon_config_get_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonConfigGetRequestBody(
        AddonConfigGetRequest { addon_id },
    ))
    .map_err(|e| JsError::new(&e))
}

/// `keys` + `values` — rownolegle wektory (len(keys) == len(values)); laczymy po indeksie.
/// wasm-bindgen nie wspiera `Vec<(String,String)>` bezposrednio, a `Vec<String>` dziala.
#[wasm_bindgen(js_name = encodeAddonConfigSetRequest)]
pub fn encode_addon_config_set_request(
    addon_id: String,
    keys: Vec<String>,
    values: Vec<String>,
) -> Result<Vec<u8>, JsError> {
    if keys.len() != values.len() {
        return Err(JsError::new("keys i values musza miec ta sama dlugosc"));
    }
    let pairs: Vec<(String, String)> = keys.into_iter().zip(values.into_iter()).collect();
    encode_body_inner(&MessageBody::AddonConfigSetRequestBody(
        AddonConfigSetRequest {
            addon_id,
            values: pairs,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAddonLogsRequest)]
pub fn encode_addon_logs_request(
    addon_id: String,
    limit: f64,
    offset: f64,
    level: Option<String>,
    search: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonLogsRequestBody(AddonLogsRequest {
        addon_id,
        limit: limit as i64,
        offset: offset as i64,
        level,
        search,
    }))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAddonToolsRequest)]
pub fn encode_addon_tools_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonToolsRequestBody(AddonToolsRequest {
        addon_id,
    }))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAddonResourcesGetRequest)]
pub fn encode_addon_resources_get_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonResourcesGetRequestBody(
        AddonResourcesGetRequest { addon_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAddonResourcesSetRequest)]
pub fn encode_addon_resources_set_request(
    addon_id: String,
    max_instances: f64,
    cpu_limit_pct: f64,
    ram_mb: f64,
    storage_mb: f64,
    http_requests_per_min: f64,
    llm_tokens_per_min: f64,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonResourcesSetRequestBody(
        AddonResourcesSetRequest {
            addon_id,
            max_instances: max_instances as i32,
            cpu_limit_pct: cpu_limit_pct as i32,
            ram_mb: ram_mb as i32,
            storage_mb: storage_mb as i32,
            http_requests_per_min: http_requests_per_min as i32,
            llm_tokens_per_min: llm_tokens_per_min as i32,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAddonNetworkRulesGetRequest)]
pub fn encode_addon_network_rules_get_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonNetworkRulesGetRequestBody(
        AddonNetworkRulesGetRequest { addon_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAddonNetworkRulesSetRequest)]
pub fn encode_addon_network_rules_set_request(
    addon_id: String,
    allowed_hosts: Vec<String>,
    blocked_hosts: Vec<String>,
    mode: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonNetworkRulesSetRequestBody(
        AddonNetworkRulesSetRequest {
            addon_id,
            allowed_hosts,
            blocked_hosts,
            mode,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAddonReloadRequest)]
pub fn encode_addon_reload_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonReloadRequestBody(AddonReloadRequest {
        addon_id,
    }))
    .map_err(|e| JsError::new(&e))
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
        assert_eq!(PROTOCOL_SCHEMA_VERSION, tentaflow_protocol::SCHEMA_VERSION);
    }

    #[test]
    fn roundtrip_envelope_with_model_list_request() {
        let body = encode_body_inner(&MessageBody::ModelListRequest).unwrap();
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
        let body = encode_body_inner(&MessageBody::ModelListRequest).unwrap();
        let frame = encode_envelope_direct_inner(1, 1, 0xF001, body).unwrap();
        assert!(rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&frame).is_ok());
        assert!(rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&[]).is_err());
        assert!(rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&[0u8; 8]).is_err());
        assert!(
            rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&frame[..frame.len() / 2]).is_err()
        );
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

// =============================================================================
// IAM encoders (users + groups + resource permissions). Zwracaja MessageBody
// bytes gotowe do envelope wrap. Kazdy encoder bierze typed args, buduje
// IamPayload i encoduje.
// =============================================================================

use tentaflow_protocol::IamPayload;

fn encode_iam(payload: IamPayload) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::IamBody(payload)).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeIamListUsersRequest)]
pub fn encode_iam_list_users() -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqListUsers)
}

#[wasm_bindgen(js_name = encodeIamGetUserRequest)]
pub fn encode_iam_get_user(user_id: f64) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqGetUser { user_id: user_id as i64 })
}

#[wasm_bindgen(js_name = encodeIamCreateUserRequest)]
pub fn encode_iam_create_user(
    username: String,
    password: String,
    display_name: String,
    email: String,
    role: String,
    group_ids_csv: String,
) -> Result<Vec<u8>, JsError> {
    let group_ids: Vec<i64> = group_ids_csv
        .split(',')
        .filter_map(|s| s.trim().parse::<i64>().ok())
        .collect();
    encode_iam(IamPayload::ReqCreateUser {
        username, password, display_name, email, role, group_ids,
    })
}

#[wasm_bindgen(js_name = encodeIamUpdateUserRequest)]
pub fn encode_iam_update_user(
    user_id: f64,
    display_name: String,
    email: String,
    is_active: bool,
    role: String,
) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqUpdateUser {
        user_id: user_id as i64, display_name, email, is_active, role,
    })
}

#[wasm_bindgen(js_name = encodeIamDeleteUserRequest)]
pub fn encode_iam_delete_user(user_id: f64) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqDeleteUser { user_id: user_id as i64 })
}

#[wasm_bindgen(js_name = encodeIamSetUserGroupsRequest)]
pub fn encode_iam_set_user_groups(user_id: f64, group_ids_csv: String) -> Result<Vec<u8>, JsError> {
    let group_ids: Vec<i64> = group_ids_csv
        .split(',')
        .filter_map(|s| s.trim().parse::<i64>().ok())
        .collect();
    encode_iam(IamPayload::ReqSetUserGroups { user_id: user_id as i64, group_ids })
}

#[wasm_bindgen(js_name = encodeIamResetUserPasswordRequest)]
pub fn encode_iam_reset_password(user_id: f64, new_password: String) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqResetUserPassword { user_id: user_id as i64, new_password })
}

#[wasm_bindgen(js_name = encodeIamListGroupsRequest)]
pub fn encode_iam_list_groups() -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqListGroups)
}

#[wasm_bindgen(js_name = encodeIamCreateGroupRequest)]
pub fn encode_iam_create_group(name: String, description: String) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqCreateGroup { name, description })
}

#[wasm_bindgen(js_name = encodeIamUpdateGroupRequest)]
pub fn encode_iam_update_group(group_id: f64, name: String, description: String) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqUpdateGroup { group_id: group_id as i64, name, description })
}

#[wasm_bindgen(js_name = encodeIamDeleteGroupRequest)]
pub fn encode_iam_delete_group(group_id: f64) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqDeleteGroup { group_id: group_id as i64 })
}

#[wasm_bindgen(js_name = encodeIamGroupMembersRequest)]
pub fn encode_iam_group_members(group_id: f64) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqGroupMembers { group_id: group_id as i64 })
}

#[wasm_bindgen(js_name = encodeIamSetPermissionRequest)]
pub fn encode_iam_set_permission(
    resource_type: String,
    resource_id: String,
    subject_type: String,
    subject_id: f64,
    access_level: String,
) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqSetPermission {
        resource_type, resource_id, subject_type,
        subject_id: subject_id as i64, access_level,
    })
}

#[wasm_bindgen(js_name = encodeIamClearPermissionRequest)]
pub fn encode_iam_clear_permission(
    resource_type: String,
    resource_id: String,
    subject_type: String,
    subject_id: f64,
) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqClearPermission {
        resource_type, resource_id, subject_type, subject_id: subject_id as i64,
    })
}

#[wasm_bindgen(js_name = encodeIamListPermsForResourceRequest)]
pub fn encode_iam_list_perms_resource(resource_type: String, resource_id: String) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqListPermsForResource { resource_type, resource_id })
}

#[wasm_bindgen(js_name = encodeIamListPermsForSubjectRequest)]
pub fn encode_iam_list_perms_subject(subject_type: String, subject_id: f64) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqListPermsForSubject { subject_type, subject_id: subject_id as i64 })
}

// =============================================================================
// Network settings encoders (interfejsy hosta + konfiguracja bind/filter).
// Wrapuja NetworkPayload w MessageBody::NetworkBody i serializuja rkyv.
// =============================================================================

use tentaflow_protocol::{NetworkConfig, NetworkInterfaceInfo, NetworkPayload};

fn encode_network(payload: NetworkPayload) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::NetworkBody(payload)).map_err(|e| JsError::new(&e))
}

/// Konwertuje pojedynczy `NetworkInterfaceInfo` na JS object dla GUI.
fn network_interface_info_to_js(iface: &NetworkInterfaceInfo) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "name", iface.name.clone().into());
    set(&obj, "mac", iface.mac.clone().into());
    let ipv4 = js_sys::Array::new();
    for addr in iface.ipv4_addrs.iter() {
        ipv4.push(&JsValue::from_str(addr));
    }
    set(&obj, "ipv4Addrs", ipv4.clone().into());
    set(&obj, "ipv4_addrs", ipv4.into());
    set(&obj, "mtu", (iface.mtu as f64).into());
    set(&obj, "kind", iface.kind.clone().into());
    set(&obj, "isUp", iface.is_up.into());
    set(&obj, "is_up", iface.is_up.into());
    set(&obj, "description", iface.description.clone().into());
    obj
}

/// Konwertuje `NetworkConfig` na JS object z polami w camelCase i snake_case
/// (parzysta dostepnosc dla istniejacych konsumentow w GUI).
fn network_config_to_js(cfg: &NetworkConfig) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "bindMode", cfg.bind_mode.clone().into());
    set(&obj, "bind_mode", cfg.bind_mode.clone().into());
    set(&obj, "bindIpv4", cfg.bind_ipv4.clone().into());
    set(&obj, "bind_ipv4", cfg.bind_ipv4.clone().into());
    set(&obj, "hideDocker", cfg.hide_docker.into());
    set(&obj, "hide_docker", cfg.hide_docker.into());
    set(&obj, "hideLinkLocal", cfg.hide_link_local.into());
    set(&obj, "hide_link_local", cfg.hide_link_local.into());
    set(&obj, "hideLoopback", cfg.hide_loopback.into());
    set(&obj, "hide_loopback", cfg.hide_loopback.into());
    set(&obj, "hideCgnat", cfg.hide_cgnat.into());
    set(&obj, "hide_cgnat", cfg.hide_cgnat.into());
    set(&obj, "preferSameSubnet", cfg.prefer_same_subnet.into());
    set(&obj, "prefer_same_subnet", cfg.prefer_same_subnet.into());
    set(&obj, "irohRelayUrl", cfg.iroh_relay_url.clone().into());
    set(&obj, "iroh_relay_url", cfg.iroh_relay_url.clone().into());
    obj
}

/// MessageBody::NetworkBody(NetworkPayload::ReqInterfacesList).
#[wasm_bindgen(js_name = encodeNetworkInterfacesListRequest)]
pub fn encode_network_interfaces_list_request() -> Result<Vec<u8>, JsError> {
    encode_network(NetworkPayload::ReqInterfacesList)
}

/// MessageBody::NetworkBody(NetworkPayload::ReqConfigGet).
#[wasm_bindgen(js_name = encodeNetworkConfigGetRequest)]
pub fn encode_network_config_get_request() -> Result<Vec<u8>, JsError> {
    encode_network(NetworkPayload::ReqConfigGet)
}

/// MessageBody::NetworkBody(NetworkPayload::ReqRelayStatus).
#[wasm_bindgen(js_name = encodeNetworkRelayStatusRequest)]
pub fn encode_network_relay_status_request() -> Result<Vec<u8>, JsError> {
    encode_network(NetworkPayload::ReqRelayStatus)
}

/// MessageBody::NetworkBody(NetworkPayload::ReqConfigUpdate(NetworkConfig { .. })).
/// Pola przekazywane jako typed args (no serde-wasm-bindgen); strony JS i WASM
/// zgodne z definicja `NetworkConfig` w `tentaflow-protocol`.
#[wasm_bindgen(js_name = encodeNetworkConfigUpdateRequest)]
pub fn encode_network_config_update_request(
    bind_mode: String,
    bind_ipv4: String,
    hide_docker: bool,
    hide_link_local: bool,
    hide_loopback: bool,
    hide_cgnat: bool,
    prefer_same_subnet: bool,
    iroh_relay_url: String,
) -> Result<Vec<u8>, JsError> {
    encode_network(NetworkPayload::ReqConfigUpdate(NetworkConfig {
        bind_mode,
        bind_ipv4,
        hide_docker,
        hide_link_local,
        hide_loopback,
        hide_cgnat,
        prefer_same_subnet,
        iroh_relay_url,
    }))
}
