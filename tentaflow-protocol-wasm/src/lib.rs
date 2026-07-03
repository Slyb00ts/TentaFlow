// =============================================================================
// Plik: tentaflow-protocol-wasm/src/lib.rs
// Opis: WASM bindings dla browser-side CBOR codec. Eksportuje encode/decode
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
    SCHEMA_VERSION as PROTOCOL_SCHEMA_VERSION,
    envelope::{Envelope, EnvelopeFlags, Routing, message_kind},
    message_body::{
        AddonAdminOnlySetRequest, AddonConfigGetRequest, AddonConfigSetRequest, AddonDetailRequest,
        AddonDocumentPayload, AddonDocumentUploadChunkRequest,
        AddonInstallRequest, AddonInstanceDuplicateRequest, AddonInstanceInstallRequest,
        AddonInstancePayload, AddonInstanceUpdateRequest, AddonInstanceVersionsRequest,
        AddonLogsRequest, AddonNetworkRulesGetRequest, AddonStoragePayload,
        AddonStorageStatsRequest, AddonVectorConfig, AddonVectorGetConfigRequest,
        AddonVectorPayload, AddonVectorServiceRef, AddonVectorSetConfigRequest,
        AddonNetworkRulesSetRequest, AddonOAuthAuthorizeStartRequest,
        AddonOAuthConfigClearSecretRequest, AddonOAuthConfigListRequest,
        AddonOAuthConfigSetRequest, AddonOAuthLinkedAccountsRequest, AddonOAuthReauthorizeRequest,
        AddonOAuthRevokeRequest, AddonOAuthTestConnectionRequest, AddonPermissionCatalogRequest,
        AddonPermissionCheckRequest, AddonPermissionDefaultSetRequest,
        AddonPermissionMatrixRequest, AddonPermissionSetRequest, AddonReloadRequest,
        AddonResourcesGetRequest, AddonResourcesSetRequest, AddonShowInCatalogSetRequest,
        AddonToggleRequest, AddonToolsRequest, AddonUninstallRequest, AddonVisibilityListRequest,
        AddonVisibilitySetRequest, AddonAccessDecisionRequest, AddonAccessListRequest,
        AliasConsumerGrantRequest, AliasConsumerListRequest, AliasConsumerRevokeRequest,
        AliasVisibilitySetRequest, ModelConsumerGrantRequest, ModelConsumerListRequest,
        ModelConsumerRevokeRequest, ModelVisibilitySetRequest,
        ApiKeyCreateRequest, AuthLoginRequest,
        BaselineAdoptPhaseTag, BaselineAdoptStartRequest, ChatMessage,
        ChatStreamRequest, ClusterAddMemberRequest, ClusterCreateRequest, ClusterDeleteRequest,
        ClusterDeployRequest, ClusterDeployStopRequest,
        ClusterDetailRequest, ClusterProbeStreamRequest, ClusterRemoveMemberRequest,
        ClusterUpdateRequest, DeployVllmRecommendRequest, SuggestServicePortRequest,
        FlowCreateRequest, FlowUpdateRequest,
        FlowVersionGetRequest, FlowVersionListRequest, FlowVersionRestoreRequest,
        MePreferencesGetRequest, MePreferencesUpdateRequest, MeshConnectRequest,
        MeshNodeCommandRequest, MeshNodeNetworkConfigRequest, MeshPairInitRequest,
        MeshPairingConfirmRequest, MeshPairingRejectRequest, MeshPairingStartRequest,
        MeshTrustRetrustRequest, MeshTrustRevokeRequest, MessageBody, ModelAliasCreateRequest,
        ModelAliasDeleteRequest, ModelAliasUpdateRequest, ModelInstallRequest,
        MyOAuthAccountsListRequest, NoteCreateRequest, NoteDeleteRequest, NoteDetailRequest,
        NoteSetPinnedRequest, NoteUpdateRequest, NotesListRequest, NotesRequest, NotesResponse,
        ProtocolError, ProtocolErrorCode, ServiceManifestDeployRequest, SettingEntry,
        SettingsUpdateRequest, SsoProviderCreateRequest, SsoProviderDeleteRequest,
        TranslateRequest, TtsRule,
    },
};
use wasm_bindgen::prelude::*;
use std::collections::HashMap;
use std::sync::OnceLock;

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

fn encode_body_inner(body: &MessageBody) -> Result<Vec<u8>, String> {
    tentaflow_protocol::cbor::encode(body).map_err(|e| format!("body encode failed: {e}"))
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

/// MessageBody::ApiKeyCreateRequest { name, key_type, subject_id, scope_resources }.
/// `scope_resources` travels as two parallel arrays (types[i] + ids[i]) so the
/// wasm-bindgen boundary stays on simple `Vec<String>` values.
#[wasm_bindgen(js_name = encodeApiKeyCreateRequest)]
pub fn encode_api_key_create_request(
    name: String,
    key_type: String,
    subject_id: Option<String>,
    scope_types: Vec<String>,
    scope_ids: Vec<String>,
) -> Result<Vec<u8>, JsError> {
    let scope_resources = scope_types
        .into_iter()
        .zip(scope_ids)
        .map(|(resource_type, resource_id)| tentaflow_protocol::ResourceRef {
            resource_type,
            resource_id,
        })
        .collect();
    encode_body_inner(&MessageBody::ApiKeyCreateRequestBody(ApiKeyCreateRequest {
        name,
        key_type,
        subject_id,
        scope_resources,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ApiKeyScopeListRequest { key_uid }.
#[wasm_bindgen(js_name = encodeApiKeyScopeListRequest)]
pub fn encode_api_key_scope_list_request(key_uid: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ApiKeyScopeListRequest { key_uid }).map_err(|e| JsError::new(&e))
}

/// MessageBody::ApiKeyScopeSetRequest { key_uid, resource_type, resource_id, access_level }.
#[wasm_bindgen(js_name = encodeApiKeyScopeSetRequest)]
pub fn encode_api_key_scope_set_request(
    key_uid: String,
    resource_type: String,
    resource_id: String,
    access_level: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ApiKeyScopeSetRequest {
        key_uid,
        resource_type,
        resource_id,
        access_level,
    })
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ApiKeyScopeClearRequest { key_uid, resource_type, resource_id }.
#[wasm_bindgen(js_name = encodeApiKeyScopeClearRequest)]
pub fn encode_api_key_scope_clear_request(
    key_uid: String,
    resource_type: String,
    resource_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ApiKeyScopeClearRequest {
        key_uid,
        resource_type,
        resource_id,
    })
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ApiKeyRotateRequest { key_uid }.
#[wasm_bindgen(js_name = encodeApiKeyRotateRequest)]
pub fn encode_api_key_rotate_request(key_uid: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ApiKeyRotateRequest { key_uid }).map_err(|e| JsError::new(&e))
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

/// MessageBody::MePreferencesGetRequest (unit variant).
#[wasm_bindgen(js_name = encodeMePreferencesGetRequest)]
pub fn encode_me_preferences_get_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MePreferencesGetRequestBody(
        MePreferencesGetRequest {},
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::MePreferencesUpdateRequest { language }.
#[wasm_bindgen(js_name = encodeMePreferencesUpdateRequest)]
pub fn encode_me_preferences_update_request(language: Option<String>) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MePreferencesUpdateRequestBody(
        MePreferencesUpdateRequest { language },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ChatStreamRequest — przyjmuje JSON string messages, parsuje
/// jako JsValue. Bootstrap accepts tylko `model_id` + jednoelementowa lista
/// user messages. Pelny messages[] input po integracji serde-wasm-bindgen (#36 ph.2).
#[wasm_bindgen(js_name = encodeChatStreamRequestSimple)]
pub fn encode_chat_stream_request_simple(
    model_id: String,
    user_message: String,
    flow_id: Option<String>,
    session_id: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ChatStreamRequestBody(ChatStreamRequest {
        model_id,
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: user_message,
        }],
        temperature: None,
        max_tokens: None,
        flow_id,
        session_id,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::FlowInvokeRequest — uniwersalny most do flow engine. Wariant
/// audio-only dla chat audio (jedno wejście Audio). Multi-input dojdzie później.
#[wasm_bindgen(js_name = encodeFlowInvokeAudio)]
#[allow(clippy::too_many_arguments)]
pub fn encode_flow_invoke_audio(
    flow_id: Option<String>,
    model: String,
    service_type: String,
    mime: String,
    sample_rate: Option<u32>,
    audio: Vec<u8>,
    language: Option<String>,
    session_id: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FlowInvokeRequestBody(
        tentaflow_protocol::FlowInvokeRequest {
            flow_id,
            model,
            service_type,
            inputs: vec![tentaflow_protocol::FlowInputValue::Audio {
                mime,
                sample_rate,
                bytes: audio,
            }],
            language,
            session_id,
        },
    ))
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
    encode_body_inner(&MessageBody::ClusterUpdateRequestBody(
        ClusterUpdateRequest {
            cluster_id,
            name,
            description,
            strategy,
            failover_enabled,
            failover_target,
            health_check_interval_ms,
            timeout_ms,
        },
    ))
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
    encode_body_inner(&MessageBody::ClusterDetailRequestBody(
        ClusterDetailRequest { cluster_id },
    ))
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
    encode_body_inner(&MessageBody::ClusterCreateRequestBody(
        ClusterCreateRequest {
            name,
            description,
            strategy,
            failover_enabled,
            failover_target,
            health_check_interval_ms,
            timeout_ms,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterDeleteRequest { cluster_id }.
#[wasm_bindgen(js_name = encodeClusterDeleteRequest)]
pub fn encode_cluster_delete_request(cluster_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterDeleteRequestBody(
        ClusterDeleteRequest { cluster_id },
    ))
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
    encode_body_inner(&MessageBody::ClusterAddMemberRequestBody(
        ClusterAddMemberRequest {
            cluster_id,
            node_id,
            interface_type,
            interface_speed_mbps,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterRemoveMemberRequest.
#[wasm_bindgen(js_name = encodeClusterRemoveMemberRequest)]
pub fn encode_cluster_remove_member_request(
    cluster_id: String,
    node_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterRemoveMemberRequestBody(
        ClusterRemoveMemberRequest {
            cluster_id,
            node_id,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterProbeStreamRequest { node_ids }.
#[wasm_bindgen(js_name = encodeClusterProbeStreamRequest)]
pub fn encode_cluster_probe_stream_request(node_ids: Vec<String>) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterProbeStreamRequestBody(
        ClusterProbeStreamRequest {
            node_ids,
            cluster_id: None,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterRdmaConfigureRequest { cluster_id, sudo_password, mtu }.
#[wasm_bindgen(js_name = encodeClusterRdmaConfigureRequest)]
pub fn encode_cluster_rdma_configure_request(
    cluster_id: String,
    sudo_password: String,
    mtu: Option<u32>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterRdmaConfigureRequestBody(
        tentaflow_protocol::ClusterRdmaConfigureRequest {
            cluster_id,
            sudo_password,
            mtu,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterDeployRequest — deploy one model split across the whole
/// cluster (vLLM tensor-parallel). Optional fields fall back to backend defaults
/// when passed `None`.
#[wasm_bindgen(js_name = encodeClusterDeployRequest)]
#[allow(clippy::too_many_arguments)]
pub fn encode_cluster_deploy_request(
    cluster_id: String,
    engine_id: String,
    model_repo: Option<String>,
    model_preset_id: Option<String>,
    served_model_name: Option<String>,
    gpu_memory_utilization: Option<f32>,
    max_model_len: Option<u32>,
    port: Option<u16>,
    gpus_per_node: Option<u32>,
    config_json: Option<String>,
    gcs_timeout_secs: Option<u32>,
    ready_timeout_secs: Option<u32>,
    // Appended last so existing positional JS calls stay valid (undefined → None
    // → backend default 600 s build budget).
    build_timeout_secs: Option<u32>,
    // Optional per-model pricing captured at deploy time (persisted for the
    // served model). Appended last so older positional JS calls stay valid.
    prompt_per_1k: Option<f64>,
    completion_per_1k: Option<f64>,
    audio_per_min: Option<f64>,
    image_each: Option<f64>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterDeployRequestBody(ClusterDeployRequest {
        cluster_id,
        engine_id,
        model_repo,
        model_preset_id,
        served_model_name,
        gpu_memory_utilization,
        max_model_len,
        port,
        gpus_per_node,
        config_json,
        build_timeout_secs,
        gcs_timeout_secs,
        ready_timeout_secs,
        prompt_per_1k,
        completion_per_1k,
        audio_per_min,
        image_each,
    }))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ClusterDeployStopRequest { cluster_id, deployment_cluster_id }.
#[wasm_bindgen(js_name = encodeClusterDeployStopRequest)]
pub fn encode_cluster_deploy_stop_request(
    cluster_id: String,
    deployment_cluster_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ClusterDeployStopRequestBody(
        ClusterDeployStopRequest {
            cluster_id,
            deployment_cluster_id,
        },
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

// ---- Sync baseline-adopt admin (FAZA C krok 3 — donor list/start/status/clear) ----

#[wasm_bindgen(js_name = encodeBaselineDonorListRequest)]
pub fn encode_baseline_donor_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BaselineDonorListRequest).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeBaselineAdoptStartRequest)]
pub fn encode_baseline_adopt_start_request(donor_node_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BaselineAdoptStartRequestBody(
        BaselineAdoptStartRequest { donor_node_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeBaselineAdoptStatusRequest)]
pub fn encode_baseline_adopt_status_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BaselineAdoptStatusRequest).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeBaselineAdoptClearRequest)]
pub fn encode_baseline_adopt_clear_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BaselineAdoptClearRequest).map_err(|e| JsError::new(&e))
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

// ---- Catalog + aliasy ----

#[wasm_bindgen(js_name = encodeCatalogListRequest)]
pub fn encode_catalog_list_request(
    surface_filter: Option<String>,
    include_blocking_diagnostics: bool,
) -> Result<Vec<u8>, JsError> {
    let body = MessageBody::CatalogListRequestBody(tentaflow_protocol::CatalogListRequest {
        surface_filter,
        include_blocking_diagnostics,
    });
    encode_body_inner(&body).map_err(|e| JsError::new(&e))
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
    encode_body_inner(&MessageBody::SettingsUpdateRequestBody(
        SettingsUpdateRequest {
            entries: vec![SettingEntry {
                key,
                value,
                is_secret,
            }],
        },
    ))
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
    default_group_id: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SsoProviderCreateRequestBody(
        SsoProviderCreateRequest {
            name,
            provider_type,
            client_id,
            client_secret,
            discovery_url,
            auto_create_users,
            default_group_id,
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
    encode_body_inner(&MessageBody::DeploymentBody(
        DeploymentPayload::ReqLogStream(DeploymentLogStreamRequest {
            deploy_id,
            replay_tail,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

/// Redeploy in-place: backend reużywa zapisany `config_json` serwisu, więc
/// frontend wysyła tylko `service_id`.
#[wasm_bindgen(js_name = encodeServiceRedeployRequest)]
pub fn encode_service_redeploy_request(service_id: f64) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::DeploymentBody(
        tentaflow_protocol::DeploymentPayload::ReqRedeploy(
            tentaflow_protocol::ServiceRedeployRequest {
                service_id: service_id as i64,
            },
        ),
    ))
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

// ---- Multi-instance: katalog pakietow + operacje na instancjach ----

fn encode_addon_instance(payload: AddonInstancePayload) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonInstanceBody(payload)).map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonInstanceBody(ReqCatalogList) — lista pakietow w katalogu.
#[wasm_bindgen(js_name = encodeAddonCatalogListRequest)]
pub fn encode_addon_catalog_list_request() -> Result<Vec<u8>, JsError> {
    encode_addon_instance(AddonInstancePayload::ReqCatalogList)
}

/// MessageBody::AddonInstanceBody(ReqInstall) — instalacja instancji z katalogu.
/// `config` is a JS `Array<[key, value]>` of install-time connection-param
/// values (e.g. the robot IP). Empty for non-robot packages.
#[wasm_bindgen(js_name = encodeAddonInstanceInstallRequest)]
pub fn encode_addon_instance_install_request(
    package_id: String,
    version: String,
    display_name: String,
    config: JsValue,
) -> Result<Vec<u8>, JsError> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    if !config.is_undefined() && !config.is_null() {
        let arr: js_sys::Array = config
            .dyn_into()
            .map_err(|_| JsError::new("config musi byc Array<[key, value]>"))?;
        for i in 0..arr.length() {
            let pair: js_sys::Array = arr
                .get(i)
                .dyn_into()
                .map_err(|_| JsError::new("config element musi byc [key, value]"))?;
            let key = pair
                .get(0)
                .as_string()
                .ok_or_else(|| JsError::new("config key musi byc string"))?;
            let value = pair
                .get(1)
                .as_string()
                .ok_or_else(|| JsError::new("config value musi byc string"))?;
            pairs.push((key, value));
        }
    }
    encode_addon_instance(AddonInstancePayload::ReqInstall(AddonInstanceInstallRequest {
        package_id,
        version,
        display_name,
        config: pairs,
    }))
}

/// MessageBody::AddonInstanceBody(ReqDuplicate) — duplikacja instancji.
#[wasm_bindgen(js_name = encodeAddonInstanceDuplicateRequest)]
pub fn encode_addon_instance_duplicate_request(
    source_addon_id: String,
    new_display_name: String,
) -> Result<Vec<u8>, JsError> {
    encode_addon_instance(AddonInstancePayload::ReqDuplicate(
        AddonInstanceDuplicateRequest {
            source_addon_id,
            new_display_name,
        },
    ))
}

/// MessageBody::AddonInstanceBody(ReqVersions) — wersje dostepne dla instancji.
#[wasm_bindgen(js_name = encodeAddonInstanceVersionsRequest)]
pub fn encode_addon_instance_versions_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_addon_instance(AddonInstancePayload::ReqVersions(
        AddonInstanceVersionsRequest { addon_id },
    ))
}

/// MessageBody::AddonInstanceBody(ReqUpdate) — hot-update instancji do wersji.
#[wasm_bindgen(js_name = encodeAddonInstanceUpdateRequest)]
pub fn encode_addon_instance_update_request(
    addon_id: String,
    target_version: String,
) -> Result<Vec<u8>, JsError> {
    encode_addon_instance(AddonInstancePayload::ReqUpdate(AddonInstanceUpdateRequest {
        addon_id,
        target_version,
    }))
}

/// MessageBody::AddonStorageBody(StatsRequest) — statystyki storage addona.
#[wasm_bindgen(js_name = encodeAddonStorageStatsRequest)]
pub fn encode_addon_storage_stats_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonStorageBody(
        AddonStoragePayload::StatsRequest(AddonStorageStatsRequest { addon_id }),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonVectorBody(GetConfigRequest) — config vector backendu addona.
#[wasm_bindgen(js_name = encodeAddonVectorGetConfigRequest)]
pub fn encode_addon_vector_get_config_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonVectorBody(
        AddonVectorPayload::GetConfigRequest(AddonVectorGetConfigRequest { addon_id }),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonVectorBody(SetConfigRequest) — zapis config vector backendu.
/// Pola configu jako osobne argumenty (bez serde_json w crate wasm).
#[wasm_bindgen(js_name = encodeAddonVectorSetConfigRequest)]
#[allow(clippy::too_many_arguments)]
pub fn encode_addon_vector_set_config_request(
    addon_id: String,
    backend: String,
    milvus_source: Option<String>,
    service_node_id: Option<String>,
    service_id: Option<String>,
    manual_uri: Option<String>,
    collection_override: Option<String>,
    milvus_user: Option<String>,
    milvus_password: Option<String>,
) -> Result<Vec<u8>, JsError> {
    let service_ref = match service_id {
        Some(sid) if !sid.is_empty() => Some(AddonVectorServiceRef {
            node_id: service_node_id.unwrap_or_default(),
            service_id: sid,
        }),
        _ => None,
    };
    let config = AddonVectorConfig {
        backend,
        milvus_source,
        service_ref,
        manual_uri,
        collection_override,
    };
    encode_body_inner(&MessageBody::AddonVectorBody(
        AddonVectorPayload::SetConfigRequest(AddonVectorSetConfigRequest {
            addon_id,
            config,
            milvus_user,
            milvus_password,
        }),
    ))
    .map_err(|e| JsError::new(&e))
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
    group_id: String,
    visible: bool,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonVisibilitySetRequestBody(
        AddonVisibilitySetRequest {
            addon_id,
            group_id,
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
    subject_id: String,
    permission_id: String,
    grant_mode: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonPermissionSetRequestBody(
        AddonPermissionSetRequest {
            addon_id,
            subject_type,
            subject_id,
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
    user_id: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonPermissionCheckRequestBody(
        AddonPermissionCheckRequest {
            addon_id,
            permission_id,
            user_id,
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
    user_id: Option<String>,
    addon_id: Option<String>,
    action: Option<String>,
    from_date: Option<String>,
    to_date: Option<String>,
    search: Option<String>,
) -> tentaflow_protocol::AuditLogFilters {
    tentaflow_protocol::AuditLogFilters {
        user_id,
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
    user_id: Option<String>,
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

#[wasm_bindgen(js_name = encodeSchedulerJobsListRequest)]
pub fn encode_scheduler_jobs_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SchedulerBody(
        tentaflow_protocol::SchedulerPayload::JobsListRequest(
            tentaflow_protocol::SchedulerJobsListRequest,
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSchedulerActionsListRequest)]
pub fn encode_scheduler_actions_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SchedulerBody(
        tentaflow_protocol::SchedulerPayload::ActionsListRequest(
            tentaflow_protocol::SchedulerActionsListRequest,
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSchedulerRunsListRequest)]
pub fn encode_scheduler_runs_list_request(job_id: String, limit: u32) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SchedulerBody(
        tentaflow_protocol::SchedulerPayload::RunsListRequest(
            tentaflow_protocol::SchedulerRunsListRequest {
                job_id,
                limit: limit.min(200),
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSchedulerJobUpsertRequest)]
pub fn encode_scheduler_job_upsert_request(job_json: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SchedulerBody(
        tentaflow_protocol::SchedulerPayload::JobUpsertRequest(
            tentaflow_protocol::SchedulerJobUpsertRequest { job_json },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSchedulerJobDeleteRequest)]
pub fn encode_scheduler_job_delete_request(job_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SchedulerBody(
        tentaflow_protocol::SchedulerPayload::JobDeleteRequest(
            tentaflow_protocol::SchedulerJobDeleteRequest { job_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSchedulerJobRunNowRequest)]
pub fn encode_scheduler_job_run_now_request(job_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SchedulerBody(
        tentaflow_protocol::SchedulerPayload::JobRunNowRequest(
            tentaflow_protocol::SchedulerJobRunNowRequest { job_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeTokenUsageSummaryRequest)]
pub fn encode_token_usage_summary_request(
    period: String,
    period_key: String,
    group_by: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::TokenUsageBody(
        tentaflow_protocol::TokenUsagePayload::UsageSummaryRequest {
            period,
            period_key,
            group_by,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeTokenListQuotasRequest)]
pub fn encode_token_list_quotas_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::TokenUsageBody(
        tentaflow_protocol::TokenUsagePayload::ListQuotasRequest,
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeTokenUpsertQuotaRequest)]
#[allow(clippy::too_many_arguments)]
pub fn encode_token_upsert_quota_request(
    id: Option<String>,
    scope_type: String,
    subject_id: Option<String>,
    model_id: Option<String>,
    period: String,
    max_total_tokens: i64,
    is_active: bool,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::TokenUsageBody(
        tentaflow_protocol::TokenUsagePayload::UpsertQuotaRequest {
            quota: tentaflow_protocol::TokenQuotaUpsertWire {
                id,
                scope_type,
                subject_id,
                model_id,
                period,
                max_total_tokens,
                is_active,
            },
        },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeTokenDeleteQuotaRequest)]
pub fn encode_token_delete_quota_request(id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::TokenUsageBody(
        tentaflow_protocol::TokenUsagePayload::DeleteQuotaRequest { id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeTokenCoordinatorStatusRequest)]
pub fn encode_token_coordinator_status_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::TokenUsageBody(
        tentaflow_protocol::TokenUsagePayload::CoordinatorStatusRequest,
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeModelMetricsSummaryRequest)]
#[allow(clippy::too_many_arguments)]
pub fn encode_model_metrics_summary_request(
    period: String,
    period_key: String,
    group_by: String,
    filter_model: Option<String>,
    filter_node: Option<String>,
    filter_service: Option<String>,
    filter_backend: Option<String>,
    filter_modality: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelMetricsBody(
        tentaflow_protocol::ModelMetricsPayload::SummaryRequest {
            period,
            period_key,
            group_by,
            filter: tentaflow_protocol::ModelMetricsFilterWire {
                model: filter_model,
                node: filter_node,
                service: filter_service,
                backend: filter_backend,
                modality: filter_modality,
            },
        },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeModelMetricsNodeServiceRequest)]
pub fn encode_model_metrics_node_service_request(
    period: String,
    period_key: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelMetricsBody(
        tentaflow_protocol::ModelMetricsPayload::NodeServiceRequest { period, period_key },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeModelMetricsPricingGet)]
pub fn encode_model_metrics_pricing_get() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelMetricsBody(
        tentaflow_protocol::ModelMetricsPayload::PricingGet,
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeModelMetricsPricingSet)]
pub fn encode_model_metrics_pricing_set(
    model_id: String,
    prompt_per_1k: f64,
    completion_per_1k: f64,
    audio_per_min: f64,
    image_each: f64,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelMetricsBody(
        tentaflow_protocol::ModelMetricsPayload::PricingSet {
            model_id,
            prompt_per_1k,
            completion_per_1k,
            audio_per_min,
            image_each,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

// ----- Benchmark Studio -----

#[wasm_bindgen(js_name = encodeBenchmarkListRequest)]
pub fn encode_benchmark_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BenchmarkBody(
        tentaflow_protocol::BenchmarkPayload::ListRequest,
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeBenchmarkGetRequest)]
pub fn encode_benchmark_get_request(id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BenchmarkBody(
        tentaflow_protocol::BenchmarkPayload::GetRequest { id },
    ))
    .map_err(|e| JsError::new(&e))
}

/// `targets_json` to JSON-owa tablica obiektów TargetInputWire (id, kind,
/// service_ref, api_type, host, port, api_key?, model, label). `api_key` obecny
/// tylko gdy użytkownik wpisał nowy sekret — nie wraca w odczycie.
#[wasm_bindgen(js_name = encodeBenchmarkSaveRequest)]
pub fn encode_benchmark_save_request(
    id: Option<String>,
    name: String,
    config_json: String,
    targets_json: String,
) -> Result<Vec<u8>, JsError> {
    let targets: Vec<tentaflow_protocol::TargetInputWire> = serde_json::from_str(&targets_json)
        .map_err(|e| JsError::new(&format!("invalid targets_json: {e}")))?;
    encode_body_inner(&MessageBody::BenchmarkBody(
        tentaflow_protocol::BenchmarkPayload::SaveRequest {
            id,
            name,
            config_json,
            targets,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeBenchmarkDeleteRequest)]
pub fn encode_benchmark_delete_request(id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BenchmarkBody(
        tentaflow_protocol::BenchmarkPayload::DeleteRequest { id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeBenchmarkStartRunRequest)]
pub fn encode_benchmark_start_run_request(benchmark_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BenchmarkBody(
        tentaflow_protocol::BenchmarkPayload::StartRunRequest { benchmark_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeBenchmarkRunStatusRequest)]
pub fn encode_benchmark_run_status_request(run_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BenchmarkBody(
        tentaflow_protocol::BenchmarkPayload::RunStatusRequest { run_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeBenchmarkRunResultsRequest)]
pub fn encode_benchmark_run_results_request(run_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BenchmarkBody(
        tentaflow_protocol::BenchmarkPayload::RunResultsRequest { run_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeBenchmarkListRunsRequest)]
pub fn encode_benchmark_list_runs_request(benchmark_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BenchmarkBody(
        tentaflow_protocol::BenchmarkPayload::ListRunsRequest { benchmark_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeBenchmarkRecentRunsRequest)]
pub fn encode_benchmark_recent_runs_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BenchmarkBody(
        tentaflow_protocol::BenchmarkPayload::RecentRunsRequest,
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeBenchmarkCancelRunRequest)]
pub fn encode_benchmark_cancel_run_request(run_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BenchmarkBody(
        tentaflow_protocol::BenchmarkPayload::CancelRunRequest { run_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeBenchmarkRunStreamRequest)]
pub fn encode_benchmark_run_stream_request(run_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::BenchmarkBody(
        tentaflow_protocol::BenchmarkPayload::RunStreamRequest { run_id },
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioProjectsListRequest)]
pub fn encode_ml_studio_projects_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ProjectsListRequest(
            tentaflow_protocol::MlStudioProjectsListRequest,
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioProjectTypesListRequest)]
pub fn encode_ml_studio_project_types_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ProjectTypesListRequest(
            tentaflow_protocol::MlStudioProjectTypesListRequest,
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioProjectCreateRequest)]
pub fn encode_ml_studio_project_create_request(
    name: String,
    description: String,
    project_type: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ProjectCreateRequest(
            tentaflow_protocol::MlStudioProjectCreateRequest {
                name,
                description,
                project_type,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioProjectDetailRequest)]
pub fn encode_ml_studio_project_detail_request(project_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ProjectDetailRequest(
            tentaflow_protocol::MlStudioProjectDetailRequest { project_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioProjectMembersListRequest)]
pub fn encode_ml_studio_project_members_list_request(
    project_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ProjectMembersListRequest(
            tentaflow_protocol::MlStudioProjectMembersListRequest { project_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioProjectInviteRequest)]
pub fn encode_ml_studio_project_invite_request(
    project_id: String,
    invitee_user_id: String,
    role: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ProjectInviteRequest(
            tentaflow_protocol::MlStudioProjectInviteRequest {
                project_id,
                invitee_user_id,
                role,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioProjectMemberRemoveRequest)]
pub fn encode_ml_studio_project_member_remove_request(
    project_id: String,
    user_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ProjectMemberRemoveRequest(
            tentaflow_protocol::MlStudioProjectMemberRemoveRequest {
                project_id,
                user_id,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioProjectMemberRoleSetRequest)]
pub fn encode_ml_studio_project_member_role_set_request(
    project_id: String,
    user_id: String,
    role: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ProjectMemberRoleSetRequest(
            tentaflow_protocol::MlStudioProjectMemberRoleSetRequest {
                project_id,
                user_id,
                role,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

/// Upload a tabular file for profiling. `bytes` arrives from JS as a Uint8Array
/// and wasm-bindgen materializes it directly into `Vec<u8>` — no base64 or copy
/// step on the JS side.
#[wasm_bindgen(js_name = encodeMlStudioDatasetUploadRequest)]
pub fn encode_ml_studio_dataset_upload_request(
    project_id: String,
    name: String,
    filename: String,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::DatasetUploadRequest(
            tentaflow_protocol::MlStudioDatasetUploadRequest {
                project_id,
                name,
                filename,
                bytes,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioDatasetUploadChunkRequest)]
pub fn encode_ml_studio_dataset_upload_chunk_request(
    project_id: String,
    name: String,
    filename: String,
    upload_id: String,
    seq: u32,
    total_chunks: u32,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::DatasetUploadChunkRequest(
            tentaflow_protocol::MlStudioDatasetUploadChunkRequest {
                project_id,
                name,
                filename,
                upload_id,
                seq,
                total_chunks,
                bytes,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonDocumentUploadChunkRequestBody — jeden fragment pliku
/// wgrywanego z panelu UI addona do jego document store. `org_id` NIE jest tu —
/// serwer bierze org z sesji. `bytes` to surowy fragment (Uint8Array).
#[wasm_bindgen(js_name = encodeAddonDocumentUploadChunkRequest)]
pub fn encode_addon_document_upload_chunk_request(
    addon_id: String,
    upload_id: String,
    filename: String,
    mime: String,
    seq: u32,
    total_chunks: u32,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonDocumentBody(
        AddonDocumentPayload::UploadChunkRequest(AddonDocumentUploadChunkRequest {
            addon_id,
            upload_id,
            filename,
            mime,
            seq,
            total_chunks,
            bytes,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioDatasetsListRequest)]
pub fn encode_ml_studio_datasets_list_request(project_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::DatasetsListRequest(
            tentaflow_protocol::MlStudioDatasetsListRequest { project_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioDatasetProfileRequest)]
pub fn encode_ml_studio_dataset_profile_request(dataset_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::DatasetProfileRequest(
            tentaflow_protocol::MlStudioDatasetProfileRequest { dataset_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioTabularTrainRequest)]
pub fn encode_ml_studio_tabular_train_request(
    project_id: String,
    dataset_id: String,
    target_column: String,
    task: String,
    engine: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::TabularTrainRequest(
            tentaflow_protocol::MlStudioTabularTrainRequest {
                project_id,
                dataset_id,
                target_column,
                task,
                // Puste/None traktujemy jak brak wyboru (domyślny silnik Rust po stronie Core).
                engine: engine.filter(|s| !s.is_empty()),
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioResourceGrantCreateRequest)]
pub fn encode_ml_studio_resource_grant_create_request(
    subject_kind: String,
    subject_id: String,
    node_id: String,
    resource_kind: String,
    resource_ref: String,
    quota: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ResourceGrantCreateRequest(
            tentaflow_protocol::MlStudioResourceGrantCreateRequest {
                subject_kind,
                subject_id,
                node_id,
                resource_kind,
                resource_ref,
                quota,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioResourceGrantsListRequest)]
pub fn encode_ml_studio_resource_grants_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ResourceGrantsListRequest(
            tentaflow_protocol::MlStudioResourceGrantsListRequest,
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioResourceGrantRevokeRequest)]
pub fn encode_ml_studio_resource_grant_revoke_request(
    grant_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ResourceGrantRevokeRequest(
            tentaflow_protocol::MlStudioResourceGrantRevokeRequest { grant_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioProjectResourcesRequest)]
pub fn encode_ml_studio_project_resources_request(project_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ProjectResourcesRequest(
            tentaflow_protocol::MlStudioProjectResourcesRequest { project_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioTrainingRunsListRequest)]
pub fn encode_ml_studio_training_runs_list_request(
    project_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::TrainingRunsListRequest(
            tentaflow_protocol::MlStudioTrainingRunsListRequest { project_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioJobsOverviewRequest)]
pub fn encode_ml_studio_jobs_overview_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::JobsOverviewRequest(
            tentaflow_protocol::MlStudioJobsOverviewRequest {},
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioModelsListRequest)]
pub fn encode_ml_studio_models_list_request(project_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ModelsListRequest(
            tentaflow_protocol::MlStudioModelsListRequest { project_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioProjectGrantsListRequest)]
pub fn encode_ml_studio_project_grants_list_request(
    project_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ProjectGrantsListRequest(
            tentaflow_protocol::MlStudioProjectGrantsListRequest { project_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioFtTrainStartRequest)]
#[allow(clippy::too_many_arguments)]
pub fn encode_ml_studio_ft_train_start_request(
    project_id: String,
    dataset_id: String,
    base_model: String,
    method: String,
    objective: String,
    teacher_model: Option<String>,
    learning_rate: f64,
    batch_size: u32,
    grad_accum_steps: u32,
    epochs: u32,
    lora_r: u32,
    lora_alpha: u32,
    lora_dropout: f64,
    max_seq_len: u32,
    merge_adapter: bool,
    target_node_id: Option<String>,
    num_gpus: u32,
    dist_nnodes: u32,
    dist_node_rank: u32,
    dist_master_addr: String,
    dist_master_port: u32,
) -> Result<Vec<u8>, JsError> {
    // nnodes>1 → trening rozproszony (multi-rig); inaczej single-node.
    let dist = if dist_nnodes > 1 {
        Some(tentaflow_protocol::MlStudioDistConfig {
            nnodes: dist_nnodes,
            node_rank: dist_node_rank,
            master_addr: dist_master_addr,
            master_port: dist_master_port,
        })
    } else {
        None
    };
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::FtTrainStartRequest(
            tentaflow_protocol::MlStudioFtTrainStartRequest {
                project_id,
                dataset_id,
                base_model,
                method,
                objective,
                teacher_model: teacher_model.filter(|s| !s.is_empty()),
                hyperparams: tentaflow_protocol::MlStudioFtHyperparams {
                    learning_rate,
                    batch_size,
                    grad_accum_steps,
                    epochs,
                    lora_r,
                    lora_alpha,
                    lora_dropout,
                    max_seq_len,
                },
                merge_adapter,
                target_node_id: target_node_id.filter(|s| !s.is_empty()),
                num_gpus: if num_gpus == 0 { None } else { Some(num_gpus) },
                dist,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioDistillGenerateRequest)]
#[allow(clippy::too_many_arguments)]
pub fn encode_ml_studio_distill_generate_request(
    project_id: String,
    dataset_name: String,
    question_source: String,
    source_dataset_id: Option<String>,
    question_field: Option<String>,
    generate_prompt: Option<String>,
    question_model: Option<String>,
    num_questions: u32,
    teacher_model: String,
    answer_instruction: Option<String>,
    temperature: f64,
    max_tokens: u32,
    objective: Option<String>,
    rejected_model: Option<String>,
    rejected_instruction: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::DistillGenerateRequest(
            tentaflow_protocol::MlStudioDistillGenerateRequest {
                project_id,
                dataset_name,
                question_source,
                source_dataset_id: source_dataset_id.filter(|s| !s.is_empty()),
                question_field: question_field.filter(|s| !s.is_empty()),
                generate_prompt: generate_prompt.filter(|s| !s.is_empty()),
                question_model: question_model.filter(|s| !s.is_empty()),
                num_questions: if num_questions == 0 {
                    None
                } else {
                    Some(num_questions)
                },
                teacher_model,
                answer_instruction: answer_instruction.filter(|s| !s.is_empty()),
                temperature: if temperature > 0.0 {
                    Some(temperature as f32)
                } else {
                    None
                },
                max_tokens: if max_tokens == 0 {
                    None
                } else {
                    Some(max_tokens)
                },
                objective: objective.filter(|s| !s.is_empty()),
                rejected_model: rejected_model.filter(|s| !s.is_empty()),
                rejected_instruction: rejected_instruction.filter(|s| !s.is_empty()),
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioDistillGenerateStatusRequest)]
pub fn encode_ml_studio_distill_generate_status_request(
    dataset_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::DistillGenerateStatusRequest(
            tentaflow_protocol::MlStudioDistillGenerateStatusRequest { dataset_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioDatasetRowsRequest)]
pub fn encode_ml_studio_dataset_rows_request(
    dataset_id: String,
    limit: u32,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::DatasetRowsRequest(
            tentaflow_protocol::MlStudioDatasetRowsRequest {
                dataset_id,
                limit: if limit == 0 { None } else { Some(limit) },
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioDatasetRowsSaveRequest)]
pub fn encode_ml_studio_dataset_rows_save_request(
    dataset_id: String,
    rows: Vec<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::DatasetRowsSaveRequest(
            tentaflow_protocol::MlStudioDatasetRowsSaveRequest { dataset_id, rows },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioFtTrainStatusRequest)]
pub fn encode_ml_studio_ft_train_status_request(run_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::FtTrainStatusRequest(
            tentaflow_protocol::MlStudioFtTrainStatusRequest { run_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioFtExportRequest)]
pub fn encode_ml_studio_ft_export_request(
    model_id: String,
    outtype: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::FtExportRequest(
            tentaflow_protocol::MlStudioFtExportRequest { model_id, outtype },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioFtExportStatusRequest)]
pub fn encode_ml_studio_ft_export_status_request(model_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::FtExportStatusRequest(
            tentaflow_protocol::MlStudioFtExportStatusRequest { model_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioFtDeployRequest)]
pub fn encode_ml_studio_ft_deploy_request(
    model_id: String,
    target_node_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::FtDeployRequest(
            tentaflow_protocol::MlStudioFtDeployRequest {
                model_id,
                target_node_id,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioFtChatRequest)]
pub fn encode_ml_studio_ft_chat_request(
    model_id: String,
    message: String,
    max_tokens: u32,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::FtChatRequest(
            tentaflow_protocol::MlStudioFtChatRequest {
                model_id,
                message,
                max_tokens,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioRecogTrainStartRequest)]
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub fn encode_ml_studio_recog_train_start_request(
    project_id: String,
    dataset_id: String,
    variant: String,
    epochs: u32,
    batch_size: u32,
    grad_accum: u32,
    learning_rate: f64,
    resolution: u32,
    early_stopping: bool,
    target_node_id: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::RecogTrainStartRequest(
            tentaflow_protocol::MlStudioRecogTrainStartRequest {
                project_id,
                dataset_id,
                variant,
                hyperparams: tentaflow_protocol::MlStudioRecogHyperparams {
                    epochs,
                    batch_size,
                    grad_accum,
                    learning_rate,
                    resolution,
                    early_stopping,
                },
                target_node_id: target_node_id.filter(|s| !s.is_empty()),
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioRecogTrainStatusRequest)]
pub fn encode_ml_studio_recog_train_status_request(run_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::RecogTrainStatusRequest(
            tentaflow_protocol::MlStudioRecogTrainStatusRequest { run_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioClassifierTrainStartRequest)]
#[allow(clippy::too_many_arguments)]
pub fn encode_ml_studio_classifier_train_start_request(
    project_id: String,
    dataset_id: String,
    attribute: String,
    source_class: String,
    variant: String,
    values: Vec<String>,
    epochs: i32,
    batch_size: i32,
    learning_rate: f32,
    image_size: i32,
    freeze_backbone: bool,
    target_node_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ClassifierTrainStartRequest(
            tentaflow_protocol::MlStudioClassifierTrainStartRequest {
                project_id,
                dataset_id,
                attribute,
                source_class,
                variant,
                values,
                hyperparams: tentaflow_protocol::MlStudioClassifierHyperparams {
                    epochs,
                    batch_size,
                    learning_rate,
                    image_size,
                    freeze_backbone,
                },
                target_node_id,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioGenericTrainStatusRequest)]
pub fn encode_ml_studio_generic_train_status_request(run_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::GenericTrainStatusRequest(
            tentaflow_protocol::MlStudioGenericTrainStatusRequest { run_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioRecogDetectRequest)]
pub fn encode_ml_studio_recog_detect_request(
    model_id: String,
    threshold: f64,
    image_b64: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::RecogDetectRequest(
            tentaflow_protocol::MlStudioRecogDetectRequest {
                model_id,
                threshold,
                image_b64,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioRecogImagesListRequest)]
pub fn encode_ml_studio_recog_images_list_request(dataset_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::RecogImagesListRequest(
            tentaflow_protocol::MlStudioRecogImagesListRequest { dataset_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioRecogImageRequest)]
pub fn encode_ml_studio_recog_image_request(
    dataset_id: String,
    image_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::RecogImageRequest(
            tentaflow_protocol::MlStudioRecogImageRequest { dataset_id, image_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioRecogSaveAnnotationsRequest)]
pub fn encode_ml_studio_recog_save_annotations_request(
    dataset_id: String,
    image_id: String,
    annotations_json: String,
    approve: bool,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::RecogSaveAnnotationsRequest(
            tentaflow_protocol::MlStudioRecogSaveAnnotationsRequest {
                dataset_id,
                image_id,
                annotations_json,
                approve,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioSchemaGetRequest)]
pub fn encode_ml_studio_schema_get_request(project_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::SchemaGetRequest(
            tentaflow_protocol::MlStudioSchemaGetRequest { project_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioSchemaSaveRequest)]
pub fn encode_ml_studio_schema_save_request(
    project_id: String,
    schema_json: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::SchemaSaveRequest(
            tentaflow_protocol::MlStudioSchemaSaveRequest {
                project_id,
                schema_json,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioLookupDictsListRequest)]
pub fn encode_ml_studio_lookup_dicts_list_request(
    project_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::LookupDictsListRequest(
            tentaflow_protocol::MlStudioLookupDictsListRequest { project_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioLookupDictSaveRequest)]
pub fn encode_ml_studio_lookup_dict_save_request(
    project_id: String,
    dict_id: String,
    name: String,
    rows_json: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::LookupDictSaveRequest(
            tentaflow_protocol::MlStudioLookupDictSaveRequest {
                project_id,
                dict_id,
                name,
                rows_json,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioLookupDictDeleteRequest)]
pub fn encode_ml_studio_lookup_dict_delete_request(dict_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::LookupDictDeleteRequest(
            tentaflow_protocol::MlStudioLookupDictDeleteRequest { dict_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioServiceModelsListRequest)]
pub fn encode_ml_studio_service_models_list_request(
    capability: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::ServiceModelsListRequest(
            tentaflow_protocol::MlStudioServiceModelsListRequest { capability },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioRecogDatasetRegisterRequest)]
pub fn encode_ml_studio_recog_dataset_register_request(
    project_id: String,
    name: String,
    path: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::RecogDatasetRegisterRequest(
            tentaflow_protocol::MlStudioRecogDatasetRegisterRequest {
                project_id,
                name,
                path,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioRecogStageMediaRequest)]
pub fn encode_ml_studio_recog_stage_media_request(
    project_id: String,
    filename: String,
    upload_id: String,
    seq: u32,
    total_chunks: u32,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::RecogStageMediaRequest(
            tentaflow_protocol::MlStudioRecogStageMediaRequest {
                project_id,
                filename,
                upload_id,
                seq,
                total_chunks,
                bytes,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioRecogBuildDatasetRequest)]
pub fn encode_ml_studio_recog_build_dataset_request(
    project_id: String,
    dataset_name: String,
    fps: u32,
    source_dir: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::RecogBuildDatasetRequest(
            tentaflow_protocol::MlStudioRecogBuildDatasetRequest {
                project_id,
                dataset_name,
                fps,
                // Pusta ścieżka traktowana jak brak — Core użyje staging.
                source_dir: source_dir.filter(|s| !s.is_empty()),
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioRecogBuildStatusRequest)]
pub fn encode_ml_studio_recog_build_status_request(build_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::RecogBuildStatusRequest(
            tentaflow_protocol::MlStudioRecogBuildStatusRequest { build_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioRecogAutolabelRequest)]
pub fn encode_ml_studio_recog_autolabel_request(
    dataset_id: String,
    threshold: f64,
    mode: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::RecogAutolabelRequest(
            tentaflow_protocol::MlStudioRecogAutolabelRequest {
                dataset_id,
                threshold,
                mode,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeMlStudioRecogAutolabelStatusRequest)]
pub fn encode_ml_studio_recog_autolabel_status_request(job_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MlStudioBody(
        tentaflow_protocol::MlStudioPayload::RecogAutolabelStatusRequest(
            tentaflow_protocol::MlStudioRecogAutolabelStatusRequest { job_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSkillsListRequest)]
pub fn encode_skills_list_request(
    tag: Option<String>,
    source: Option<String>,
    status: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::ListRequest(tentaflow_protocol::SkillsListRequest {
            tag,
            source,
            status,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSkillsDetailRequest)]
pub fn encode_skills_detail_request(skill_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::DetailRequest(tentaflow_protocol::SkillsDetailRequest {
            skill_id,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSkillsUpsertRequest)]
pub fn encode_skills_upsert_request(skill_json: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::UpsertRequest(tentaflow_protocol::SkillsUpsertRequest {
            skill_json,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSkillsDeleteRequest)]
pub fn encode_skills_delete_request(skill_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::DeleteRequest(tentaflow_protocol::SkillsDeleteRequest {
            skill_id,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSkillsForkRequest)]
pub fn encode_skills_fork_request(skill_id: String, new_name: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::ForkRequest(tentaflow_protocol::SkillsForkRequest {
            skill_id,
            new_name,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSkillsHubSearchRequest)]
pub fn encode_skills_hub_search_request(
    query: String,
    source: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::HubSearchRequest(
            tentaflow_protocol::SkillsHubSearchRequest { query, source },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSkillsHubImportRequest)]
pub fn encode_skills_hub_import_request(
    source: String,
    git_ref: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::HubImportRequest(
            tentaflow_protocol::SkillsHubImportRequest { source, git_ref },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSkillsHubApproveRequest)]
pub fn encode_skills_hub_approve_request(skill_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::HubApproveRequest(
            tentaflow_protocol::SkillsHubApproveRequest { skill_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSkillsHubRejectRequest)]
pub fn encode_skills_hub_reject_request(skill_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::HubRejectRequest(
            tentaflow_protocol::SkillsHubRejectRequest { skill_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSkillsCuratorRunRequest)]
pub fn encode_skills_curator_run_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::CuratorRunRequest(
            tentaflow_protocol::SkillsCuratorRunRequest {},
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSkillsCuratorApplyRequest)]
pub fn encode_skills_curator_apply_request(
    snapshot_id: String,
    approved_actions_json: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::CuratorApplyRequest(
            tentaflow_protocol::SkillsCuratorApplyRequest {
                snapshot_id,
                approved_actions_json,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSkillsCuratorRollbackRequest)]
pub fn encode_skills_curator_rollback_request(snapshot_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::CuratorRollbackRequest(
            tentaflow_protocol::SkillsCuratorRollbackRequest { snapshot_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAgentsListRequest)]
pub fn encode_agents_list_request(
    enabled: Option<bool>,
    routable: Option<bool>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::ListRequest(tentaflow_protocol::AgentsListRequest {
            enabled,
            routable,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAgentsDetailRequest)]
pub fn encode_agents_detail_request(agent_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::DetailRequest(tentaflow_protocol::AgentsDetailRequest {
            agent_id,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAgentsUpsertRequest)]
pub fn encode_agents_upsert_request(agent_json: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::UpsertRequest(tentaflow_protocol::AgentsUpsertRequest {
            agent_json,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAgentsDeleteRequest)]
pub fn encode_agents_delete_request(agent_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::DeleteRequest(tentaflow_protocol::AgentsDeleteRequest {
            agent_id,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAgentRunsListRequest)]
pub fn encode_agent_runs_list_request(
    agent_id: Option<String>,
    status: Option<String>,
    parent_run_id: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::RunsListRequest(tentaflow_protocol::AgentRunsListRequest {
            agent_id,
            status,
            parent_run_id,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAgentRunDetailRequest)]
pub fn encode_agent_run_detail_request(run_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::RunDetailRequest(
            tentaflow_protocol::AgentRunDetailRequest { run_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeToolsCatalogRequest)]
pub fn encode_tools_catalog_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::ToolsCatalogRequest(
            tentaflow_protocol::ToolsCatalogRequest {},
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAgentRunReplyRequest)]
pub fn encode_agent_run_reply_request(
    run_id: String,
    question_id: String,
    answer: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::RunReplyRequest(
            tentaflow_protocol::AgentRunReplyRequest {
                run_id,
                question_id,
                answer,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAgentPermissionReplyRequest)]
pub fn encode_agent_permission_reply_request(
    run_id: String,
    request_id: String,
    decision: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::PermissionReplyRequest(
            tentaflow_protocol::AgentPermissionReplyRequest {
                run_id,
                request_id,
                decision,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeAgentRunCancelRequest)]
pub fn encode_agent_run_cancel_request(run_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::RunCancelRequest(
            tentaflow_protocol::AgentRunCancelRequest { run_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

/// Subscribe to a run-events scope. `scope_kind` is "session" or "run";
/// `scope_id` is the session id or run id respectively.
#[wasm_bindgen(js_name = encodeAgentRunEventsSubscribeRequest)]
pub fn encode_agent_run_events_subscribe_request(
    scope_kind: String,
    scope_id: String,
) -> Result<Vec<u8>, JsError> {
    let scope = match scope_kind.as_str() {
        "session" => tentaflow_protocol::AgentRunEventScope::Session {
            session_id: scope_id,
        },
        "run" => tentaflow_protocol::AgentRunEventScope::Run { run_id: scope_id },
        _ => return Err(JsError::new("scope_kind must be 'session' or 'run'")),
    };
    encode_body_inner(&MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::RunEventsSubscribeRequest(
            tentaflow_protocol::AgentRunEventsSubscribeRequest { scope },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

fn parse_sync_conflict_resolution(
    resolution: &str,
) -> Result<tentaflow_protocol::SyncConflictResolution, JsError> {
    match resolution {
        "keep_local" => Ok(tentaflow_protocol::SyncConflictResolution::KeepLocal),
        "ignore" => Ok(tentaflow_protocol::SyncConflictResolution::Ignore),
        "accept_remote" => Ok(tentaflow_protocol::SyncConflictResolution::AcceptRemote),
        _ => Err(JsError::new("invalid sync conflict resolution")),
    }
}

#[wasm_bindgen(js_name = encodeSyncConflictsListRequest)]
pub fn encode_sync_conflicts_list_request(
    org_id: String,
    addon_id: String,
    status: String,
    limit: u32,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SyncConflictBody(
        tentaflow_protocol::SyncConflictPayload::ListRequest(
            tentaflow_protocol::SyncConflictsListRequest {
                org_id,
                addon_id,
                status,
                limit: limit.min(500),
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSyncConflictResolveRequest)]
pub fn encode_sync_conflict_resolve_request(
    org_id: String,
    addon_id: String,
    operation_id: String,
    resolution: String,
) -> Result<Vec<u8>, JsError> {
    let resolution = parse_sync_conflict_resolution(&resolution)?;
    encode_body_inner(&MessageBody::SyncConflictBody(
        tentaflow_protocol::SyncConflictPayload::ResolveRequest(
            tentaflow_protocol::SyncConflictResolveRequest {
                org_id,
                addon_id,
                operation_id,
                resolution,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSyncStorageReportRequest)]
pub fn encode_sync_storage_report_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::SyncStorageBody(
        tentaflow_protocol::SyncStoragePayload::ReportRequest(
            tentaflow_protocol::SyncStorageReportRequest,
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AuditLogExportRequest — eksport CSV z filtrami.
#[wasm_bindgen(js_name = encodeAuditLogExportRequest)]
pub fn encode_audit_log_export_request(
    user_id: Option<String>,
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

/// MessageBody::FlowCreateRequest { name, description, graph_json,
/// published_model_name? }. `published_model_name = None` keeps the flow
/// private; passing a value publishes it on `/v1/models` after the
/// catalog rebuild — collisions with aliases / existing flows are
/// rejected by the handler before the row is written.
#[wasm_bindgen(js_name = encodeFlowCreateRequest)]
pub fn encode_flow_create_request(
    name: String,
    description: Option<String>,
    graph_json: String,
    published_model_name: Option<String>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FlowCreateRequestBody(FlowCreateRequest {
        name,
        description,
        graph_json,
        published_model_name,
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

/// MessageBody::FlowUpdateRequest — partial update flow. Pass
/// `publish_set=true, published_model_name=Some("foo")` to publish or
/// `publish_set=true, published_model_name=None` to un-publish; leave
/// `publish_set=false` to keep whatever the server has.
#[wasm_bindgen(js_name = encodeFlowUpdateRequest)]
pub fn encode_flow_update_request(
    flow_id: String,
    name: Option<String>,
    description: Option<String>,
    flow_json: Option<String>,
    status: Option<String>,
    publish_set: bool,
    published_model_name: Option<String>,
) -> Result<Vec<u8>, JsError> {
    let published_model_name = if publish_set {
        Some(published_model_name)
    } else {
        None
    };
    encode_body_inner(&MessageBody::FlowUpdateRequestBody(FlowUpdateRequest {
        flow_id,
        name,
        description,
        flow_json,
        status,
        published_model_name,
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
    encode_body_inner(&MessageBody::FlowVersionListRequestBody(
        FlowVersionListRequest { flow_id },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::FlowVersionGetRequest { flow_id, version_id }.
#[wasm_bindgen(js_name = encodeFlowVersionGetRequest)]
pub fn encode_flow_version_get_request(
    flow_id: String,
    version_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::FlowVersionGetRequestBody(
        FlowVersionGetRequest {
            flow_id,
            version_id,
        },
    ))
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

// --- Services (Krok N2 — packed in `MessageBody::ServiceBody`) -----------

/// MessageBody::ServiceBody(ServicePayload::ReqList). Empty filter values are
/// treated as "no filter".
#[wasm_bindgen(js_name = encodeServiceListRequest)]
pub fn encode_service_list_request(
    engine_id_filter: Option<String>,
    category_filter: Option<String>,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{ServiceListRequest, ServicePayload};
    encode_body_inner(&MessageBody::ServiceBody(ServicePayload::ReqList(
        ServiceListRequest {
            engine_id_filter: engine_id_filter.filter(|s| !s.is_empty()),
            category_filter: category_filter.filter(|s| !s.is_empty()),
        },
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceBody(ServicePayload::ReqDelete) — stop + delete the row
/// (cascades to `model_registry`).
#[wasm_bindgen(js_name = encodeServiceDeleteRequest)]
pub fn encode_service_delete_request(
    service_id: f64,
    node_id: Option<String>,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{ServiceDeleteRequest, ServicePayload};
    encode_body_inner(&MessageBody::ServiceBody(ServicePayload::ReqDelete(
        ServiceDeleteRequest {
            service_id: service_id as i64,
            node_id,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceBody(ServicePayload::ReqPin) — toggles the pin flag
/// used by the supervisor for auto-respawn.
#[wasm_bindgen(js_name = encodeServicePinRequest)]
pub fn encode_service_pin_request(
    service_id: f64,
    pinned: bool,
    node_id: Option<String>,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{ServicePayload, ServicePinRequest};
    encode_body_inner(&MessageBody::ServiceBody(ServicePayload::ReqPin(
        ServicePinRequest {
            service_id: service_id as i64,
            pinned,
            node_id,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceBody(ServicePayload::ReqStart) — unpause + spawn the
/// engine when stopped/failed/paused. Idempotent for already-running services.
#[wasm_bindgen(js_name = encodeServiceStartRequest)]
pub fn encode_service_start_request(
    service_id: f64,
    node_id: Option<String>,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{ServicePayload, ServiceStartRequest};
    encode_body_inner(&MessageBody::ServiceBody(ServicePayload::ReqStart(
        ServiceStartRequest {
            service_id: service_id as i64,
            node_id,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceBody(ServicePayload::ReqPause) — supervisor leaves a
/// paused service untouched.
#[wasm_bindgen(js_name = encodeServicePauseRequest)]
pub fn encode_service_pause_request(
    service_id: f64,
    paused: bool,
    node_id: Option<String>,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{ServicePauseRequest, ServicePayload};
    encode_body_inner(&MessageBody::ServiceBody(ServicePayload::ReqPause(
        ServicePauseRequest {
            service_id: service_id as i64,
            paused,
            node_id,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceBody(ServicePayload::ReqUpdate) — edycja serwisu po
/// deploy (Edit modal). 13 pól opcjonalnych; klient sam decyduje co jest
/// `Some(_)`. Payload przyjmujemy jako JSON string żeby nie trzymać 13
/// argumentów wasm-bindgen.
#[wasm_bindgen(js_name = encodeServiceConfigUpdateRequest)]
pub fn encode_service_config_update_request(payload_json: String) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{ServicePayload, ServiceUpdateRequest};
    let payload: ServiceUpdateRequest = serde_json::from_str(&payload_json)
        .map_err(|e| JsError::new(&format!("ServiceUpdateRequest JSON: {e}")))?;
    encode_body_inner(&MessageBody::ServiceBody(ServicePayload::ReqUpdate(
        payload,
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceBody(ServicePayload::ReqVramHint) — snapshot VRAM
/// per GPU + lista zewnętrznych procesów (sunshine, chrome itp.).
#[wasm_bindgen(js_name = encodeServiceVramHintRequest)]
pub fn encode_service_vram_hint_request(
    gpu_index: Option<u32>,
    node_id: Option<String>,
    exclude_service_id: Option<f64>,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{ServicePayload, ServiceVramHintRequest};
    encode_body_inner(&MessageBody::ServiceBody(ServicePayload::ReqVramHint(
        ServiceVramHintRequest {
            gpu_index,
            node_id,
            exclude_service_id: exclude_service_id.map(|v| v as i64),
        },
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceBody(ServicePayload::ReqEnginePresets) — lista
/// presetów modelu z manifestu silnika (single source of truth z
/// `tentaflow-containers/<cat>/_services/<engine>.toml`).
#[wasm_bindgen(js_name = encodeServiceEnginePresetsRequest)]
pub fn encode_service_engine_presets_request(engine_id: String) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{ServiceEnginePresetsRequest, ServicePayload};
    encode_body_inner(&MessageBody::ServiceBody(ServicePayload::ReqEnginePresets(
        ServiceEnginePresetsRequest { engine_id },
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceBody(ServicePayload::ReqModelCatalog) — live model
/// catalog of a deployed external provider service (fetched from provider API).
#[wasm_bindgen(js_name = encodeServiceModelCatalogRequest)]
pub fn encode_service_model_catalog_request(payload_json: String) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{ServiceModelCatalogRequest, ServicePayload};
    let payload: ServiceModelCatalogRequest = serde_json::from_str(&payload_json)
        .map_err(|e| JsError::new(&format!("ServiceModelCatalogRequest JSON: {e}")))?;
    encode_body_inner(&MessageBody::ServiceBody(ServicePayload::ReqModelCatalog(
        payload,
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceBody(ServicePayload::ReqModelSelection) — persist the
/// admin's model selection (model_registry upserted to exactly this set).
#[wasm_bindgen(js_name = encodeServiceModelSelectionRequest)]
pub fn encode_service_model_selection_request(payload_json: String) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{ServiceModelSelectionRequest, ServicePayload};
    let payload: ServiceModelSelectionRequest = serde_json::from_str(&payload_json)
        .map_err(|e| JsError::new(&format!("ServiceModelSelectionRequest JSON: {e}")))?;
    encode_body_inner(&MessageBody::ServiceBody(ServicePayload::ReqModelSelection(
        payload,
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceBody(ServicePayload::ReqOauthStart) — begin a
/// subscription OAuth login (browser PKCE) on the named node.
#[wasm_bindgen(js_name = encodeServiceOauthStartRequest)]
pub fn encode_service_oauth_start_request(payload_json: String) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{ServiceOauthStartRequest, ServicePayload};
    let payload: ServiceOauthStartRequest = serde_json::from_str(&payload_json)
        .map_err(|e| JsError::new(&format!("ServiceOauthStartRequest JSON: {e}")))?;
    encode_body_inner(&MessageBody::ServiceBody(ServicePayload::ReqOauthStart(
        payload,
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ServiceBody(ServicePayload::ReqOauthPoll) — poll a login
/// flow's status.
#[wasm_bindgen(js_name = encodeServiceOauthPollRequest)]
pub fn encode_service_oauth_poll_request(payload_json: String) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{ServiceOauthPollRequest, ServicePayload};
    let payload: ServiceOauthPollRequest = serde_json::from_str(&payload_json)
        .map_err(|e| JsError::new(&format!("ServiceOauthPollRequest JSON: {e}")))?;
    encode_body_inner(&MessageBody::ServiceBody(ServicePayload::ReqOauthPoll(
        payload,
    )))
    .map_err(|e| JsError::new(&e))
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
    encode_body_inner(&MessageBody::PromptDetailRequest { prompt_id }).map_err(|e| JsError::new(&e))
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
    MeetingSummariesListRequest, MeetingTranscriptExportRequest, MeetingTranscriptsListRequest,
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
pub fn encode_meeting_transcripts_list(session_id: f64, since_ms: f64) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::MeetingBody(
        MeetingPayload::ReqTranscriptsList(MeetingTranscriptsListRequest {
            session_id: session_id as i64,
            since_ms: since_ms as i64,
        }),
    ))
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
    encode_body_inner(&MessageBody::MeetingBody(
        MeetingPayload::ReqSettingsUpdate(MeetingSettingsUpdateRequest { settings: kvs }),
    ))
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
    encode_body_inner(&MessageBody::MeetingBody(
        MeetingPayload::ReqActionItemsList(MeetingActionItemsListRequest {
            meeting_key,
            status_filter,
        }),
    ))
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
    encode_body_inner(&MessageBody::MeetingBody(
        MeetingPayload::ReqTranscriptExport(MeetingTranscriptExportRequest { meeting_key }),
    ))
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
    encode_body_inner(&MessageBody::TtsRuleDeleteRequest { rule_id }).map_err(|e| JsError::new(&e))
}

/// MessageBody::TtsPreviewRequest { text, model, voice } — podglad TTS
/// (synteza tekstu po czyszczeniu do audio, odtwarzane w panelu).
#[wasm_bindgen(js_name = encodeTtsPreviewRequest)]
pub fn encode_tts_preview_request(
    text: String,
    model: String,
    voice: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::TtsPreviewRequest { text, model, voice })
        .map_err(|e| JsError::new(&e))
}

// --- PII rules ------------------------------------------------------------

/// MessageBody::PiiRuleBody(ListRequest) — wire-compat z dawnym
/// PiiRuleListRequest, JS API niezmienione.
#[wasm_bindgen(js_name = encodePiiRuleListRequest)]
pub fn encode_pii_rule_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::PiiRuleBody(
        tentaflow_protocol::PiiRulePayload::ListRequest,
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::VisionBody(InferRequest) — encoder Vision inference.
#[wasm_bindgen(js_name = encodeVisionInferRequest)]
pub fn encode_vision_infer_request(
    service_name: String,
    image: Vec<u8>,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<Vec<u8>, JsError> {
    let format = match (width, height) {
        (Some(w), Some(h)) => tentaflow_protocol::VisionImageFormat::RawRgb {
            width: w,
            height: h,
        },
        _ => tentaflow_protocol::VisionImageFormat::Encoded,
    };
    let req = tentaflow_protocol::VisionInferRequest {
        service_name,
        image,
        format,
    };
    encode_body_inner(&MessageBody::VisionBody(
        tentaflow_protocol::VisionInferPayload::InferRequest(req),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::RerankBody(Request) — encoder rerankingu (Tier 1). `topN`
/// opcjonalne (None = wszystkie dokumenty).
#[wasm_bindgen(js_name = encodeRerankRequest)]
pub fn encode_rerank_request(
    model: String,
    query: String,
    documents: Vec<String>,
    top_n: Option<u32>,
    return_documents: bool,
) -> Result<Vec<u8>, JsError> {
    let req = tentaflow_protocol::RerankRequest {
        model,
        query,
        documents,
        top_n,
        return_documents,
    };
    encode_body_inner(&MessageBody::RerankBody(
        tentaflow_protocol::RerankExchange::Request(req),
    ))
    .map_err(|e| JsError::new(&e))
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
    encode_body_inner(&MessageBody::SettingsUpdateRequestBody(
        SettingsUpdateRequest { entries },
    ))
    .map_err(|e| JsError::new(&e))
}

// --- Model / alias access control (F1a §6.6) ------------------------------

/// MessageBody::AliasConsumerListRequest { alias_id }.
#[wasm_bindgen(js_name = encodeAliasConsumerListRequest)]
pub fn encode_alias_consumer_list_request(alias_id: f64) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AliasConsumerListRequestBody(
        AliasConsumerListRequest {
            alias_id: alias_id as i64,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AliasConsumerGrantRequest { alias_id, addon_id }.
#[wasm_bindgen(js_name = encodeAliasConsumerGrantRequest)]
pub fn encode_alias_consumer_grant_request(
    alias_id: f64,
    addon_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AliasConsumerGrantRequestBody(
        AliasConsumerGrantRequest {
            alias_id: alias_id as i64,
            addon_id,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AliasConsumerRevokeRequest { alias_id, addon_id }.
#[wasm_bindgen(js_name = encodeAliasConsumerRevokeRequest)]
pub fn encode_alias_consumer_revoke_request(
    alias_id: f64,
    addon_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AliasConsumerRevokeRequestBody(
        AliasConsumerRevokeRequest {
            alias_id: alias_id as i64,
            addon_id,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AliasVisibilitySetRequest { alias_id, visibility }.
#[wasm_bindgen(js_name = encodeAliasVisibilitySetRequest)]
pub fn encode_alias_visibility_set_request(
    alias_id: f64,
    visibility: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AliasVisibilitySetRequestBody(
        AliasVisibilitySetRequest {
            alias_id: alias_id as i64,
            visibility,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ModelVisibilityListRequest (unit variant).
#[wasm_bindgen(js_name = encodeModelVisibilityListRequest)]
pub fn encode_model_visibility_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelVisibilityListRequest).map_err(|e| JsError::new(&e))
}

/// MessageBody::ModelVisibilitySetRequest { model_id, visibility }.
#[wasm_bindgen(js_name = encodeModelVisibilitySetRequest)]
pub fn encode_model_visibility_set_request(
    model_id: String,
    visibility: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelVisibilitySetRequestBody(
        ModelVisibilitySetRequest {
            model_id,
            visibility,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ModelConsumerListRequest { model_id }.
#[wasm_bindgen(js_name = encodeModelConsumerListRequest)]
pub fn encode_model_consumer_list_request(model_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelConsumerListRequestBody(
        ModelConsumerListRequest { model_id },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ModelConsumerGrantRequest { model_id, addon_id }.
#[wasm_bindgen(js_name = encodeModelConsumerGrantRequest)]
pub fn encode_model_consumer_grant_request(
    model_id: String,
    addon_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelConsumerGrantRequestBody(
        ModelConsumerGrantRequest { model_id, addon_id },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::ModelConsumerRevokeRequest { model_id, addon_id }.
#[wasm_bindgen(js_name = encodeModelConsumerRevokeRequest)]
pub fn encode_model_consumer_revoke_request(
    model_id: String,
    addon_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ModelConsumerRevokeRequestBody(
        ModelConsumerRevokeRequest { model_id, addon_id },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonAccessListRequest { addon_id }.
#[wasm_bindgen(js_name = encodeAddonAccessListRequest)]
pub fn encode_addon_access_list_request(addon_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonAccessListRequestBody(
        AddonAccessListRequest { addon_id },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonAccessDecisionRequest { addon_id, kind, target, decision }.
#[wasm_bindgen(js_name = encodeAddonAccessDecisionRequest)]
pub fn encode_addon_access_decision_request(
    addon_id: String,
    kind: String,
    target: String,
    decision: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonAccessDecisionRequestBody(
        AddonAccessDecisionRequest {
            addon_id,
            kind,
            target,
            decision,
        },
    ))
    .map_err(|e| JsError::new(&e))
}

// =============================================================================
// MessageBody decode (zwraca JS object z variant tag + polami)
// =============================================================================

fn set(obj: &js_sys::Object, key: &str, value: JsValue) {
    let _ = js_sys::Reflect::set(obj, &key.into(), &value);
}

/// Maps an optional float to a JS number or `null` (so absent classification /
/// regression metrics arrive as `null`, not `0`).
fn opt_f64_to_js(value: Option<f64>) -> JsValue {
    match value {
        Some(v) => v.into(),
        None => JsValue::NULL,
    }
}

fn string_vec_to_js(values: Vec<String>) -> js_sys::Array {
    let arr = js_sys::Array::new();
    for v in values {
        arr.push(&JsValue::from(v));
    }
    arr
}

fn sync_conflict_resolution_to_str(
    resolution: tentaflow_protocol::SyncConflictResolution,
) -> &'static str {
    match resolution {
        tentaflow_protocol::SyncConflictResolution::KeepLocal => "keep_local",
        tentaflow_protocol::SyncConflictResolution::Ignore => "ignore",
        tentaflow_protocol::SyncConflictResolution::AcceptRemote => "accept_remote",
    }
}

fn sync_storage_level_to_str(level: tentaflow_protocol::SyncStoragePressureLevel) -> &'static str {
    match level {
        tentaflow_protocol::SyncStoragePressureLevel::Ok => "ok",
        tentaflow_protocol::SyncStoragePressureLevel::Info => "info",
        tentaflow_protocol::SyncStoragePressureLevel::Warning => "warning",
        tentaflow_protocol::SyncStoragePressureLevel::Critical => "critical",
        tentaflow_protocol::SyncStoragePressureLevel::Unknown => "unknown",
    }
}

fn set_optional_u64(obj: &js_sys::Object, key: &str, value: Option<u64>) {
    if let Some(value) = value {
        set(obj, key, value.clone().into());
    }
}

/// Buduje JS obiekt z `AddonVectorConfig` (camelCase) dla pickera vector backendu.
fn vector_config_to_js(c: &tentaflow_protocol::AddonVectorConfig) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "backend", c.backend.clone().into());
    if let Some(s) = &c.milvus_source {
        set(&o, "milvusSource", s.clone().into());
    }
    if let Some(sr) = &c.service_ref {
        let r = js_sys::Object::new();
        set(&r, "nodeId", sr.node_id.clone().into());
        set(&r, "serviceId", sr.service_id.clone().into());
        set(&o, "serviceRef", r.into());
    }
    if let Some(u) = &c.manual_uri {
        set(&o, "manualUri", u.clone().into());
    }
    if let Some(co) = &c.collection_override {
        set(&o, "collectionOverride", co.clone().into());
    }
    o.into()
}

fn set_optional_u32(obj: &js_sys::Object, key: &str, value: Option<u32>) {
    if let Some(value) = value {
        set(obj, key, value.clone().into());
    }
}

fn set_optional_i64(obj: &js_sys::Object, key: &str, value: Option<i64>) {
    if let Some(value) = value {
        set(obj, key, value.clone().into());
    }
}

fn set_optional_string(obj: &js_sys::Object, key: &str, value: Option<String>) {
    if let Some(value) = value {
        set(obj, key, value.into());
    }
}

/// Build a JS object from one `AccessConsumerEntry` (alias/model consumer grant
/// row). Optional fields become `null` when `None`. Both camelCase and
/// snake_case keys are emitted, mirroring the ModelListResponse precedent.
fn access_consumer_entry_to_js(c: &tentaflow_protocol::AccessConsumerEntry) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "addonId", c.addon_id.clone().into());
    set(&o, "addon_id", c.addon_id.clone().into());
    match c.granted_by_user_id {
        Some(v) => {
            set(&o, "grantedByUserId", v.into());
            set(&o, "granted_by_user_id", v.into());
        }
        None => {
            set(&o, "grantedByUserId", JsValue::NULL);
            set(&o, "granted_by_user_id", JsValue::NULL);
        }
    }
    match c.granted_at {
        Some(v) => {
            set(&o, "grantedAt", v.into());
            set(&o, "granted_at", v.into());
        }
        None => {
            set(&o, "grantedAt", JsValue::NULL);
            set(&o, "granted_at", JsValue::NULL);
        }
    }
    match c.revoked_at {
        Some(v) => {
            set(&o, "revokedAt", v.into());
            set(&o, "revoked_at", v.into());
        }
        None => {
            set(&o, "revokedAt", JsValue::NULL);
            set(&o, "revoked_at", JsValue::NULL);
        }
    }
    o.into()
}

/// Build a JS object from one `AddonUsesEntry` (per-addon `uses_alias` /
/// `uses_model` declaration with reconciled grant state).
fn addon_uses_entry_to_js(u: &tentaflow_protocol::AddonUsesEntry) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "target", u.target.clone().into());
    set(&o, "required", u.required.into());
    set(&o, "reason", u.reason.clone().into());
    set(&o, "grantStatus", u.grant_status.clone().into());
    set(&o, "grant_status", u.grant_status.clone().into());
    set(&o, "ownerVisibility", u.owner_visibility.clone().into());
    set(&o, "owner_visibility", u.owner_visibility.clone().into());
    o.into()
}

/// Build a JS object from one `AccessTransition` (dependent `uses_*` row whose
/// grant_status flipped as a side effect of a mutation).
fn access_transition_to_js(t: &tentaflow_protocol::AccessTransition) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "addonId", t.addon_id.clone().into());
    set(&o, "addon_id", t.addon_id.clone().into());
    set(&o, "before", t.before.clone().into());
    set(&o, "after", t.after.clone().into());
    o.into()
}

/// Buduje jeden wiersz summary (JS object) z `ModelMetricsRowWire`. Wspoldzielony
/// przez `rows` i `grandTotal`, zeby oba mialy identyczny ksztalt (klucze
/// camelCase + snake_case, percentyle number|null).
fn model_metrics_summary_row_to_js(
    r: &tentaflow_protocol::ModelMetricsRowWire,
) -> js_sys::Object {
    let item = js_sys::Object::new();
    set(&item, "key", r.key.clone().into());
    set(&item, "promptTokens", (r.prompt_tokens as f64).into());
    set(&item, "prompt_tokens", (r.prompt_tokens as f64).into());
    set(&item, "completionTokens", (r.completion_tokens as f64).into());
    set(&item, "completion_tokens", (r.completion_tokens as f64).into());
    set(&item, "totalTokens", (r.total_tokens as f64).into());
    set(&item, "total_tokens", (r.total_tokens as f64).into());
    set(&item, "embeddingTokens", (r.embedding_tokens as f64).into());
    set(&item, "embedding_tokens", (r.embedding_tokens as f64).into());
    set(&item, "audioMs", (r.audio_ms as f64).into());
    set(&item, "audio_ms", (r.audio_ms as f64).into());
    set(&item, "images", (r.images as f64).into());
    set(&item, "requestCount", (r.request_count as f64).into());
    set(&item, "request_count", (r.request_count as f64).into());
    set(&item, "successCount", (r.success_count as f64).into());
    set(&item, "success_count", (r.success_count as f64).into());
    set(&item, "errorCount", (r.error_count as f64).into());
    set(&item, "error_count", (r.error_count as f64).into());
    set(&item, "cost", r.cost.into());
    set(&item, "missingPricing", r.missing_pricing.into());
    set(&item, "missing_pricing", r.missing_pricing.into());
    set(&item, "errorRate", r.error_rate.into());
    set(&item, "error_rate", r.error_rate.into());
    set(&item, "ttftP50", opt_f64_to_js(r.ttft_p50));
    set(&item, "ttft_p50", opt_f64_to_js(r.ttft_p50));
    set(&item, "ttftP90", opt_f64_to_js(r.ttft_p90));
    set(&item, "ttft_p90", opt_f64_to_js(r.ttft_p90));
    set(&item, "ttftP99", opt_f64_to_js(r.ttft_p99));
    set(&item, "ttft_p99", opt_f64_to_js(r.ttft_p99));
    set(&item, "decodeP50", opt_f64_to_js(r.decode_p50));
    set(&item, "decode_p50", opt_f64_to_js(r.decode_p50));
    set(&item, "decodeP90", opt_f64_to_js(r.decode_p90));
    set(&item, "decode_p90", opt_f64_to_js(r.decode_p90));
    set(&item, "decodeP99", opt_f64_to_js(r.decode_p99));
    set(&item, "decode_p99", opt_f64_to_js(r.decode_p99));
    set(&item, "e2eP50", opt_f64_to_js(r.e2e_p50));
    set(&item, "e2e_p50", opt_f64_to_js(r.e2e_p50));
    set(&item, "e2eP90", opt_f64_to_js(r.e2e_p90));
    set(&item, "e2e_p90", opt_f64_to_js(r.e2e_p90));
    set(&item, "e2eP99", opt_f64_to_js(r.e2e_p99));
    set(&item, "e2e_p99", opt_f64_to_js(r.e2e_p99));
    item
}

/// Decode helper for `MessageBody::ModelMetricsBody`. Response variants become
/// per-row JS objects; percentyle jako number|null (brak próbek = null). Klucze
/// emitowane w camelCase i snake_case, jak w torze token_usage.
fn decode_model_metrics_payload(
    obj: &js_sys::Object,
    payload: tentaflow_protocol::ModelMetricsPayload,
) {
    use tentaflow_protocol::ModelMetricsPayload as MP;
    match payload {
        MP::SummaryRequest { .. } => set(obj, "variant", "ModelMetricsSummaryRequest".into()),
        MP::NodeServiceRequest { .. } => {
            set(obj, "variant", "ModelMetricsNodeServiceRequest".into())
        }
        MP::PricingGet => set(obj, "variant", "ModelMetricsPricingGet".into()),
        MP::PricingSet { .. } => set(obj, "variant", "ModelMetricsPricingSet".into()),
        MP::SummaryResponse { rows, grand_total } => {
            set(obj, "variant", "ModelMetricsSummaryResponse".into());
            let arr = js_sys::Array::new();
            for r in &rows {
                arr.push(&model_metrics_summary_row_to_js(r));
            }
            set(obj, "rows", arr.into());
            match grand_total {
                Some(r) => set(obj, "grandTotal", model_metrics_summary_row_to_js(&r).into()),
                None => set(obj, "grandTotal", JsValue::NULL),
            }
        }
        MP::NodeServiceResponse { rows } => {
            set(obj, "variant", "ModelMetricsNodeServiceResponse".into());
            let arr = js_sys::Array::new();
            for r in rows {
                let item = js_sys::Object::new();
                set(&item, "nodeId", r.node_id.clone().into());
                set(&item, "node_id", r.node_id.into());
                set(&item, "serviceKey", r.service_key.clone().into());
                set(&item, "service_key", r.service_key.into());
                set(&item, "backend", r.backend.into());
                set(&item, "modelId", r.model_id.clone().into());
                set(&item, "model_id", r.model_id.into());
                set(&item, "promptTokens", (r.prompt_tokens as f64).into());
                set(&item, "prompt_tokens", (r.prompt_tokens as f64).into());
                set(&item, "completionTokens", (r.completion_tokens as f64).into());
                set(&item, "completion_tokens", (r.completion_tokens as f64).into());
                set(&item, "totalTokens", (r.total_tokens as f64).into());
                set(&item, "total_tokens", (r.total_tokens as f64).into());
                set(&item, "requestCount", (r.request_count as f64).into());
                set(&item, "request_count", (r.request_count as f64).into());
                set(&item, "successCount", (r.success_count as f64).into());
                set(&item, "success_count", (r.success_count as f64).into());
                set(&item, "errorCount", (r.error_count as f64).into());
                set(&item, "error_count", (r.error_count as f64).into());
                set(&item, "errorRate", r.error_rate.into());
                set(&item, "error_rate", r.error_rate.into());
                set(&item, "ttftP50", opt_f64_to_js(r.ttft_p50));
                set(&item, "ttft_p50", opt_f64_to_js(r.ttft_p50));
                set(&item, "ttftP90", opt_f64_to_js(r.ttft_p90));
                set(&item, "ttft_p90", opt_f64_to_js(r.ttft_p90));
                set(&item, "ttftP99", opt_f64_to_js(r.ttft_p99));
                set(&item, "ttft_p99", opt_f64_to_js(r.ttft_p99));
                set(&item, "decodeP50", opt_f64_to_js(r.decode_p50));
                set(&item, "decode_p50", opt_f64_to_js(r.decode_p50));
                set(&item, "decodeP90", opt_f64_to_js(r.decode_p90));
                set(&item, "decode_p90", opt_f64_to_js(r.decode_p90));
                set(&item, "decodeP99", opt_f64_to_js(r.decode_p99));
                set(&item, "decode_p99", opt_f64_to_js(r.decode_p99));
                arr.push(&item);
            }
            set(obj, "rows", arr.into());
        }
        MP::PricingList { rows } => {
            set(obj, "variant", "ModelMetricsPricingList".into());
            let arr = js_sys::Array::new();
            for r in rows {
                let item = js_sys::Object::new();
                set(&item, "modelId", r.model_id.clone().into());
                set(&item, "model_id", r.model_id.into());
                set(&item, "promptPer1k", r.prompt_per_1k.into());
                set(&item, "prompt_per_1k", r.prompt_per_1k.into());
                set(&item, "completionPer1k", r.completion_per_1k.into());
                set(&item, "completion_per_1k", r.completion_per_1k.into());
                set(&item, "audioPerMin", r.audio_per_min.into());
                set(&item, "audio_per_min", r.audio_per_min.into());
                set(&item, "imageEach", r.image_each.into());
                set(&item, "image_each", r.image_each.into());
                set(&item, "updatedAt", r.updated_at.clone().into());
                set(&item, "updated_at", r.updated_at.into());
                arr.push(&item);
            }
            set(obj, "rows", arr.into());
        }
        MP::PricingSetResult { ok, error } => {
            set(obj, "variant", "ModelMetricsPricingSetResult".into());
            set(obj, "ok", ok.into());
            match error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
        }
    }
}

fn benchmark_run_summary_to_js(r: &tentaflow_protocol::RunSummaryWire) -> JsValue {
    let item = js_sys::Object::new();
    set(&item, "id", r.id.clone().into());
    set(&item, "benchmarkId", r.benchmark_id.clone().into());
    set(&item, "benchmark_id", r.benchmark_id.clone().into());
    match &r.benchmark_name {
        Some(n) => {
            set(&item, "benchmarkName", n.clone().into());
            set(&item, "benchmark_name", n.clone().into());
        }
        None => {
            set(&item, "benchmarkName", JsValue::NULL);
            set(&item, "benchmark_name", JsValue::NULL);
        }
    }
    set(&item, "startedAt", r.started_at.clone().into());
    set(&item, "started_at", r.started_at.clone().into());
    match &r.finished_at {
        Some(f) => {
            set(&item, "finishedAt", f.clone().into());
            set(&item, "finished_at", f.clone().into());
        }
        None => {
            set(&item, "finishedAt", JsValue::NULL);
            set(&item, "finished_at", JsValue::NULL);
        }
    }
    set(&item, "status", r.status.clone().into());
    match &r.error {
        Some(e) => set(&item, "error", e.clone().into()),
        None => set(&item, "error", JsValue::NULL),
    }
    item.into()
}

fn benchmark_target_to_js(t: &tentaflow_protocol::TargetWire) -> JsValue {
    let item = js_sys::Object::new();
    set(&item, "id", t.id.clone().into());
    set(&item, "kind", t.kind.clone().into());
    match &t.service_ref {
        Some(s) => {
            set(&item, "serviceRef", s.clone().into());
            set(&item, "service_ref", s.clone().into());
        }
        None => {
            set(&item, "serviceRef", JsValue::NULL);
            set(&item, "service_ref", JsValue::NULL);
        }
    }
    set(&item, "apiType", t.api_type.clone().into());
    set(&item, "api_type", t.api_type.clone().into());
    set(&item, "host", t.host.clone().into());
    set(&item, "port", (t.port as f64).into());
    set(&item, "hasKey", t.has_key.into());
    set(&item, "has_key", t.has_key.into());
    set(&item, "model", t.model.clone().into());
    set(&item, "label", t.label.clone().into());
    item.into()
}

fn benchmark_result_row_to_js(r: &tentaflow_protocol::ResultRowWire) -> JsValue {
    let item = js_sys::Object::new();
    set(&item, "targetId", r.target_id.clone().into());
    set(&item, "target_id", r.target_id.clone().into());
    set(&item, "targetLabel", r.target_label.clone().into());
    set(&item, "target_label", r.target_label.clone().into());
    set(&item, "scenario", r.scenario.clone().into());
    set(&item, "variantJson", r.variant_json.clone().into());
    set(&item, "variant_json", r.variant_json.clone().into());
    set(&item, "ttftMsMean", opt_f64_to_js(r.ttft_ms_mean));
    set(&item, "ttft_ms_mean", opt_f64_to_js(r.ttft_ms_mean));
    set(&item, "ttftMsSigma", opt_f64_to_js(r.ttft_ms_sigma));
    set(&item, "ttft_ms_sigma", opt_f64_to_js(r.ttft_ms_sigma));
    set(&item, "prefillTpsMean", opt_f64_to_js(r.prefill_tps_mean));
    set(&item, "prefill_tps_mean", opt_f64_to_js(r.prefill_tps_mean));
    set(&item, "prefillTpsSigma", opt_f64_to_js(r.prefill_tps_sigma));
    set(&item, "prefill_tps_sigma", opt_f64_to_js(r.prefill_tps_sigma));
    set(&item, "decodeTpsMean", opt_f64_to_js(r.decode_tps_mean));
    set(&item, "decode_tps_mean", opt_f64_to_js(r.decode_tps_mean));
    set(&item, "decodeTpsSigma", opt_f64_to_js(r.decode_tps_sigma));
    set(&item, "decode_tps_sigma", opt_f64_to_js(r.decode_tps_sigma));
    set(&item, "totalMsMean", opt_f64_to_js(r.total_ms_mean));
    set(&item, "total_ms_mean", opt_f64_to_js(r.total_ms_mean));
    set(&item, "totalMsSigma", opt_f64_to_js(r.total_ms_sigma));
    set(&item, "total_ms_sigma", opt_f64_to_js(r.total_ms_sigma));
    set(&item, "p50Ms", opt_f64_to_js(r.p50_ms));
    set(&item, "p50_ms", opt_f64_to_js(r.p50_ms));
    set(&item, "p90Ms", opt_f64_to_js(r.p90_ms));
    set(&item, "p90_ms", opt_f64_to_js(r.p90_ms));
    set(&item, "p99Ms", opt_f64_to_js(r.p99_ms));
    set(&item, "p99_ms", opt_f64_to_js(r.p99_ms));
    set(&item, "requests", (r.requests as f64).into());
    set(&item, "errors", (r.errors as f64).into());
    set(&item, "samplesJson", r.samples_json.clone().into());
    set(&item, "samples_json", r.samples_json.clone().into());
    item.into()
}

/// Decode helper for `MessageBody::BenchmarkBody`. Response i stream chunki
/// stają się obiektami JS z kluczami camelCase i snake_case. Sekrety (api_key)
/// nigdy nie występują — target niesie tylko `hasKey`.
fn decode_benchmark_payload(obj: &js_sys::Object, payload: tentaflow_protocol::BenchmarkPayload) {
    use tentaflow_protocol::BenchmarkPayload as BP;
    match payload {
        BP::ListRequest => set(obj, "variant", "BenchmarkListRequest".into()),
        BP::GetRequest { .. } => set(obj, "variant", "BenchmarkGetRequest".into()),
        BP::SaveRequest { .. } => set(obj, "variant", "BenchmarkSaveRequest".into()),
        BP::DeleteRequest { .. } => set(obj, "variant", "BenchmarkDeleteRequest".into()),
        BP::StartRunRequest { .. } => set(obj, "variant", "BenchmarkStartRunRequest".into()),
        BP::RunStatusRequest { .. } => set(obj, "variant", "BenchmarkRunStatusRequest".into()),
        BP::RunResultsRequest { .. } => set(obj, "variant", "BenchmarkRunResultsRequest".into()),
        BP::ListRunsRequest { .. } => set(obj, "variant", "BenchmarkListRunsRequest".into()),
        BP::RecentRunsRequest => set(obj, "variant", "BenchmarkRecentRunsRequest".into()),
        BP::CancelRunRequest { .. } => set(obj, "variant", "BenchmarkCancelRunRequest".into()),
        BP::RunStreamRequest { .. } => set(obj, "variant", "BenchmarkRunStreamRequest".into()),
        BP::ListResponse { benchmarks } => {
            set(obj, "variant", "BenchmarkListResponse".into());
            let arr = js_sys::Array::new();
            for b in &benchmarks {
                let item = js_sys::Object::new();
                set(&item, "id", b.id.clone().into());
                set(&item, "name", b.name.clone().into());
                set(&item, "targetCount", (b.target_count as f64).into());
                set(&item, "target_count", (b.target_count as f64).into());
                set(&item, "testCount", (b.test_count as f64).into());
                set(&item, "test_count", (b.test_count as f64).into());
                let models = js_sys::Array::new();
                for m in &b.models {
                    models.push(&JsValue::from_str(m));
                }
                set(&item, "models", models.into());
                match &b.last_run {
                    Some(r) => {
                        set(&item, "lastRun", benchmark_run_summary_to_js(r));
                        set(&item, "last_run", benchmark_run_summary_to_js(r));
                    }
                    None => {
                        set(&item, "lastRun", JsValue::NULL);
                        set(&item, "last_run", JsValue::NULL);
                    }
                }
                arr.push(&item);
            }
            set(obj, "benchmarks", arr.into());
        }
        BP::GetResponse { benchmark } => {
            set(obj, "variant", "BenchmarkGetResponse".into());
            let item = js_sys::Object::new();
            set(&item, "id", benchmark.id.clone().into());
            set(&item, "name", benchmark.name.clone().into());
            set(&item, "configJson", benchmark.config_json.clone().into());
            set(&item, "config_json", benchmark.config_json.clone().into());
            set(&item, "createdAt", benchmark.created_at.clone().into());
            set(&item, "created_at", benchmark.created_at.clone().into());
            set(&item, "updatedAt", benchmark.updated_at.clone().into());
            set(&item, "updated_at", benchmark.updated_at.clone().into());
            let targets = js_sys::Array::new();
            for t in &benchmark.targets {
                targets.push(&benchmark_target_to_js(t));
            }
            set(&item, "targets", targets.into());
            set(obj, "benchmark", item.into());
        }
        BP::SaveResponse { id } => {
            set(obj, "variant", "BenchmarkSaveResponse".into());
            set(obj, "id", id.into());
        }
        BP::DeleteResult { ok } => {
            set(obj, "variant", "BenchmarkDeleteResult".into());
            set(obj, "ok", ok.into());
        }
        BP::StartRunResponse { run_id } => {
            set(obj, "variant", "BenchmarkStartRunResponse".into());
            set(obj, "runId", run_id.clone().into());
            set(obj, "run_id", run_id.into());
        }
        BP::RunStatusResponse {
            run_id,
            status,
            error,
            started_at,
            finished_at,
        } => {
            set(obj, "variant", "BenchmarkRunStatusResponse".into());
            set(obj, "runId", run_id.clone().into());
            set(obj, "run_id", run_id.into());
            set(obj, "status", status.into());
            match error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
            set(obj, "startedAt", started_at.clone().into());
            set(obj, "started_at", started_at.into());
            match finished_at {
                Some(f) => {
                    set(obj, "finishedAt", f.clone().into());
                    set(obj, "finished_at", f.into());
                }
                None => {
                    set(obj, "finishedAt", JsValue::NULL);
                    set(obj, "finished_at", JsValue::NULL);
                }
            }
        }
        BP::RunResultsResponse { results } => {
            set(obj, "variant", "BenchmarkRunResultsResponse".into());
            let arr = js_sys::Array::new();
            for r in &results {
                arr.push(&benchmark_result_row_to_js(r));
            }
            set(obj, "results", arr.into());
        }
        BP::ListRunsResponse { runs } => {
            set(obj, "variant", "BenchmarkListRunsResponse".into());
            let arr = js_sys::Array::new();
            for r in &runs {
                arr.push(&benchmark_run_summary_to_js(r));
            }
            set(obj, "runs", arr.into());
        }
        BP::RecentRunsResponse { runs } => {
            set(obj, "variant", "BenchmarkRecentRunsResponse".into());
            let arr = js_sys::Array::new();
            for r in &runs {
                arr.push(&benchmark_run_summary_to_js(r));
            }
            set(obj, "runs", arr.into());
        }
        BP::CancelRunResult { ok } => {
            set(obj, "variant", "BenchmarkCancelRunResult".into());
            set(obj, "ok", ok.into());
        }
        BP::RunStreamChunk {
            run_id,
            kind,
            phase,
            line,
            progress_pct,
            ts_ms,
        } => {
            set(obj, "variant", "BenchmarkRunStreamChunk".into());
            set(obj, "runId", run_id.clone().into());
            set(obj, "run_id", run_id.into());
            set(obj, "kind", kind.into());
            set(obj, "phase", phase.into());
            set(obj, "line", line.into());
            set(obj, "progressPct", (progress_pct as f64).into());
            set(obj, "progress_pct", (progress_pct as f64).into());
            set(obj, "tsMs", (ts_ms as f64).into());
            set(obj, "ts_ms", (ts_ms as f64).into());
        }
        BP::RunStreamEnd {
            run_id,
            status,
            error,
        } => {
            set(obj, "variant", "BenchmarkRunStreamEnd".into());
            set(obj, "runId", run_id.clone().into());
            set(obj, "run_id", run_id.into());
            set(obj, "status", status.into());
            match error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
        }
    }
}

/// Decode helper for `MessageBody::ServiceBody` (Krok N2). Splits the inner
/// `ServicePayload` enum into per-variant JS objects with snake_case fields
/// matching the Rust struct names. Both camelCase and snake_case keys are
/// emitted so the JS side can pick whichever convention it already uses.
fn decode_service_payload(obj: &js_sys::Object, payload: tentaflow_protocol::ServicePayload) {
    use tentaflow_protocol::ServicePayload as SP;
    match payload {
        SP::ReqList(r) => {
            set(obj, "variant", "ServiceListRequest".into());
            if let Some(f) = r.engine_id_filter {
                set(obj, "engineIdFilter", f.into());
            }
            if let Some(f) = r.category_filter {
                set(obj, "categoryFilter", f.into());
            }
        }
        SP::ResList(r) => {
            set(obj, "variant", "ServiceListResponse".into());
            let arr = js_sys::Array::new();
            for s in r.services {
                let item = js_sys::Object::new();
                set(&item, "id", s.id.clone().into());
                set(&item, "nodeId", s.node_id.clone().into());
                set(&item, "node_id", s.node_id.into());
                set(&item, "engineId", s.engine_id.clone().into());
                set(&item, "engine_id", s.engine_id.into());
                set(&item, "category", s.category.into());
                set(&item, "displayName", s.display_name.clone().into());
                set(&item, "display_name", s.display_name.into());
                set(&item, "deployMethod", s.deploy_method.clone().into());
                set(&item, "deploy_method", s.deploy_method.into());
                set(&item, "transport", s.transport.into());
                set(&item, "status", s.status.into());
                set(&item, "pinned", s.pinned.into());
                set(&item, "paused", s.paused.into());
                if let Some(pid) = s.runtime_pid {
                    set(&item, "runtimePid", pid.clone().into());
                    set(&item, "runtime_pid", pid.clone().into());
                }
                if let Some(p) = s.runtime_port {
                    set(&item, "runtimePort", (p as u32).into());
                    set(&item, "runtime_port", (p as u32).into());
                }
                if let Some(p) = s.sidecar_quic_port {
                    set(&item, "sidecarQuicPort", (p as u32).into());
                    set(&item, "sidecar_quic_port", (p as u32).into());
                }
                if let Some(url) = s.endpoint_url {
                    set(&item, "endpointUrl", url.clone().into());
                    set(&item, "endpoint_url", url.into());
                }
                set(&item, "restartCount", s.restart_count.into());
                set(&item, "restart_count", s.restart_count.into());
                if let Some(err) = s.health_last_err {
                    set(&item, "healthLastErr", err.clone().into());
                    set(&item, "health_last_err", err.into());
                }
                set(&item, "activeDeployId", s.active_deploy_id.clone().into());
                set(&item, "active_deploy_id", s.active_deploy_id.into());
                set(&item, "lastDeployId", s.last_deploy_id.clone().into());
                set(&item, "last_deploy_id", s.last_deploy_id.into());
                set(
                    &item,
                    "deploymentProgressPct",
                    s.deployment_progress_pct.into(),
                );
                set(
                    &item,
                    "deployment_progress_pct",
                    s.deployment_progress_pct.into(),
                );
                if let Some(msg) = s.progress_message {
                    set(&item, "progressMessage", msg.clone().into());
                    set(&item, "progress_message", msg.into());
                }
                set(&item, "createdAt", s.created_at.clone().into());
                set(&item, "created_at", s.created_at.into());
                set(&item, "updatedAt", s.updated_at.clone().into());
                set(&item, "updated_at", s.updated_at.into());
                set(&item, "updateAvailable", s.update_available.into());
                set(&item, "update_available", s.update_available.into());
                set(&item, "gpuSelection", s.gpu_selection.clone().into());
                set(&item, "gpu_selection", s.gpu_selection.into());

                let models = js_sys::Array::new();
                for m in s.models {
                    let m_item = js_sys::Object::new();
                    set(&m_item, "modelName", m.model_name.clone().into());
                    set(&m_item, "model_name", m.model_name.into());
                    if let Some(d) = m.display_name {
                        set(&m_item, "displayName", d.clone().into());
                        set(&m_item, "display_name", d.into());
                    }
                    let caps = js_sys::Array::new();
                    for c in m.capabilities {
                        caps.push(&JsValue::from_str(&c));
                    }
                    set(&m_item, "capabilities", caps.into());
                    if let Some(ctx) = m.context_length {
                        set(&m_item, "contextLength", ctx.into());
                        set(&m_item, "context_length", ctx.into());
                    }
                    if let Some(q) = m.quantization {
                        set(&m_item, "quantization", q.into());
                    }
                    set(&m_item, "isDefault", m.is_default.into());
                    set(&m_item, "is_default", m.is_default.into());
                    models.push(&m_item.into());
                }
                set(&item, "models", models.into());
                arr.push(&item.into());
            }
            set(obj, "services", arr.into());
        }
        SP::ReqDelete(r) => {
            set(obj, "variant", "ServiceDeleteRequest".into());
            set(obj, "serviceId", r.service_id.clone().into());
            set(obj, "service_id", r.service_id.clone().into());
            if let Some(n) = r.node_id {
                set(obj, "nodeId", n.clone().into());
                set(obj, "node_id", n.into());
            }
        }
        SP::ResDelete(r) => {
            set(obj, "variant", "ServiceDeleteResponse".into());
            set(obj, "success", r.success.into());
            if let Some(e) = r.error {
                set(obj, "error", e.into());
            }
        }
        SP::ReqPin(r) => {
            set(obj, "variant", "ServicePinRequest".into());
            set(obj, "serviceId", r.service_id.clone().into());
            set(obj, "service_id", r.service_id.clone().into());
            set(obj, "pinned", r.pinned.into());
            if let Some(n) = r.node_id {
                set(obj, "nodeId", n.clone().into());
                set(obj, "node_id", n.into());
            }
        }
        SP::ResPin(r) => {
            set(obj, "variant", "ServicePinResponse".into());
            set(obj, "success", r.success.into());
            if let Some(e) = r.error {
                set(obj, "error", e.into());
            }
        }
        SP::ReqPause(r) => {
            set(obj, "variant", "ServicePauseRequest".into());
            set(obj, "serviceId", r.service_id.clone().into());
            set(obj, "service_id", r.service_id.clone().into());
            set(obj, "paused", r.paused.into());
            if let Some(n) = r.node_id {
                set(obj, "nodeId", n.clone().into());
                set(obj, "node_id", n.into());
            }
        }
        SP::ResPause(r) => {
            set(obj, "variant", "ServicePauseResponse".into());
            set(obj, "success", r.success.into());
            if let Some(e) = r.error {
                set(obj, "error", e.into());
            }
        }
        SP::ReqStart(r) => {
            set(obj, "variant", "ServiceStartRequest".into());
            set(obj, "serviceId", r.service_id.clone().into());
            set(obj, "service_id", r.service_id.clone().into());
            if let Some(n) = r.node_id {
                set(obj, "nodeId", n.clone().into());
                set(obj, "node_id", n.into());
            }
        }
        SP::ResStart(r) => {
            set(obj, "variant", "ServiceStartResponse".into());
            set(obj, "success", r.success.into());
            if let Some(e) = r.error {
                set(obj, "error", e.into());
            }
        }
        SP::ReqUpdate(_) => {
            // Klient nie odbiera tego variantu (request-only); decoder zwraca
            // pustą obwiednię żeby debugger miał variant tag.
            set(obj, "variant", "ServiceConfigUpdateRequest".into());
        }
        SP::ResUpdate(r) => {
            set(obj, "variant", "ServiceConfigUpdateResponse".into());
            set(obj, "success", r.success.into());
            set(obj, "restarted", r.restarted.into());
            if let Some(e) = r.error {
                set(obj, "error", e.into());
            }
        }
        SP::ReqVramHint(_) => {
            set(obj, "variant", "ServiceVramHintRequest".into());
        }
        SP::ResVramHint(r) => {
            set(obj, "variant", "ServiceVramHintResponse".into());
            if let Some(rec) = r.recommended_utilization {
                set(obj, "recommendedUtilization", rec.clone().into());
                set(obj, "recommended_utilization", rec.clone().into());
            }
            let arr = js_sys::Array::new();
            for g in r.gpus {
                let item = js_sys::Object::new();
                set(&item, "gpuIndex", g.gpu_index.clone().into());
                set(&item, "gpu_index", g.gpu_index.clone().into());
                set(&item, "gpuName", g.gpu_name.clone().into());
                set(&item, "gpu_name", g.gpu_name.into());
                set(&item, "totalMib", g.total_mib.clone().into());
                set(&item, "total_mib", g.total_mib.clone().into());
                set(&item, "freeMib", g.free_mib.clone().into());
                set(&item, "free_mib", g.free_mib.clone().into());
                set(&item, "usedMib", g.used_mib.clone().into());
                set(&item, "used_mib", g.used_mib.clone().into());
                let procs = js_sys::Array::new();
                for p in g.external_processes {
                    let pi = js_sys::Object::new();
                    set(&pi, "pid", p.pid.clone().into());
                    set(&pi, "processName", p.process_name.clone().into());
                    set(&pi, "process_name", p.process_name.into());
                    set(&pi, "usedMib", p.used_mib.clone().into());
                    set(&pi, "used_mib", p.used_mib.clone().into());
                    procs.push(&pi);
                }
                set(&item, "externalProcesses", procs.clone().into());
                set(&item, "external_processes", procs.into());
                arr.push(&item);
            }
            set(obj, "gpus", arr.into());
        }
        SP::ReqEnginePresets(r) => {
            set(obj, "variant", "ServiceEnginePresetsRequest".into());
            set(obj, "engineId", r.engine_id.clone().into());
            set(obj, "engine_id", r.engine_id.into());
        }
        SP::ResEnginePresets(r) => {
            set(obj, "variant", "ServiceEnginePresetsResponse".into());
            let arr = js_sys::Array::new();
            for p in r.presets {
                let item = js_sys::Object::new();
                set(&item, "id", p.id.clone().into());
                set(&item, "displayName", p.display_name.clone().into());
                set(&item, "display_name", p.display_name.into());
                set(&item, "repo", p.repo.into());
                if let Some(q) = p.quantization {
                    set(&item, "quantization", q.into());
                }
                set(&item, "recommended", p.recommended.into());
                arr.push(&item);
            }
            set(obj, "presets", arr.into());
        }
        SP::ReqModelCatalog(r) => {
            set(obj, "variant", "ServiceModelCatalogRequest".into());
            set(obj, "serviceId", r.service_id.into());
            set(obj, "service_id", r.service_id.into());
            if let Some(n) = r.node_id {
                set(obj, "nodeId", n.clone().into());
                set(obj, "node_id", n.into());
            }
        }
        SP::ResModelCatalog(r) => {
            set(obj, "variant", "ServiceModelCatalogResponse".into());
            let arr = js_sys::Array::new();
            for m in r.models {
                let item = js_sys::Object::new();
                set(&item, "id", m.id.into());
                if let Some(d) = m.display_name {
                    set(&item, "displayName", d.clone().into());
                    set(&item, "display_name", d.into());
                }
                set(&item, "modality", m.modality.into());
                if let Some(c) = m.context_length {
                    set(&item, "contextLength", c.into());
                    set(&item, "context_length", c.into());
                }
                set(&item, "selected", m.selected.into());
                arr.push(&item);
            }
            set(obj, "models", arr.into());
            if let Some(e) = r.error {
                set(obj, "error", e.into());
            }
        }
        SP::ReqModelSelection(r) => {
            set(obj, "variant", "ServiceModelSelectionRequest".into());
            set(obj, "serviceId", r.service_id.into());
            set(obj, "service_id", r.service_id.into());
        }
        SP::ResModelSelection(r) => {
            set(obj, "variant", "ServiceModelSelectionResponse".into());
            set(obj, "success", r.success.into());
            if let Some(e) = r.error {
                set(obj, "error", e.into());
            }
        }
        SP::ReqOauthStart(r) => {
            set(obj, "variant", "ServiceOauthStartRequest".into());
            set(obj, "provider", r.provider.into());
        }
        SP::ResOauthStart(r) => {
            set(obj, "variant", "ServiceOauthStartResponse".into());
            set(obj, "flowId", r.flow_id.clone().into());
            set(obj, "flow_id", r.flow_id.into());
            set(obj, "authorizeUrl", r.authorize_url.clone().into());
            set(obj, "authorize_url", r.authorize_url.into());
            set(obj, "userCode", r.user_code.clone().into());
            set(obj, "user_code", r.user_code.into());
            if let Some(e) = r.error {
                set(obj, "error", e.into());
            }
        }
        SP::ReqOauthPoll(r) => {
            set(obj, "variant", "ServiceOauthPollRequest".into());
            set(obj, "flowId", r.flow_id.clone().into());
            set(obj, "flow_id", r.flow_id.into());
        }
        SP::ResOauthPoll(r) => {
            set(obj, "variant", "ServiceOauthPollResponse".into());
            set(obj, "status", r.status.into());
            if let Some(a) = r.account_label {
                set(obj, "accountLabel", a.clone().into());
                set(obj, "account_label", a.into());
            }
            if let Some(e) = r.error {
                set(obj, "error", e.into());
            }
        }
    }
}

/// Dekoduje CBOR-zakodowany MessageBody na JS object.
/// Dla znanych variantow zwraca obiekt z polem `variant`, a dla nieznanego
/// variantu `{ variant: "Unknown" }`.
#[wasm_bindgen(js_name = decodeMessageBody)]
pub fn decode_message_body(bytes: &[u8]) -> Result<JsValue, JsError> {
    let body = tentaflow_protocol::cbor::decode::<MessageBody>(bytes)
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
            asset_build_hash,
        } => {
            set(&obj, "variant", "MetaSchemaVersionAck".into());
            set(&obj, "serverVersion", (server_version as u32).into());
            set(&obj, "accepted", accepted.into());
            set(&obj, "assetBuildHash", asset_build_hash.as_str().into());
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
                set(&item, "modelName", m.model_name.clone().into());
                set(&item, "model_name", m.model_name.into());
                set(&item, "displayName", m.display_name.clone().into());
                set(&item, "display_name", m.display_name.into());
                set(&item, "category", m.category.into());
                set(&item, "engineId", m.engine_id.clone().into());
                set(&item, "engine_id", m.engine_id.into());
                set(&item, "serviceId", m.service_id.clone().into());
                set(&item, "service_id", m.service_id.clone().into());
                set(&item, "nodeId", m.node_id.clone().into());
                set(&item, "node_id", m.node_id.into());
                set(&item, "availability", m.availability.into());
                set(&item, "transport", m.transport.into());
                if let Some(url) = m.endpoint_url {
                    set(&item, "endpointUrl", url.clone().into());
                    set(&item, "endpoint_url", url.into());
                }
                let caps = js_sys::Array::new();
                for c in m.capabilities {
                    caps.push(&JsValue::from_str(&c));
                }
                set(&item, "capabilities", caps.into());
                if let Some(ctx) = m.context_length {
                    set(&item, "contextLength", ctx.into());
                    set(&item, "context_length", ctx.into());
                }
                if let Some(q) = m.quantization {
                    set(&item, "quantization", q.into());
                }
                set(&item, "isDefault", m.is_default.into());
                set(&item, "is_default", m.is_default.into());
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
                set(&item, "keyType", k.key_type.into());
                if let Some(sid) = k.subject_id {
                    set(&item, "subjectId", sid.into());
                }
                if let Some(lbl) = k.subject_label {
                    set(&item, "subjectLabel", lbl.into());
                }
                set(&item, "scopeCount", k.scope_count.into());
                set(&item, "isActive", k.is_active.into());
                arr.push(&item.into());
            }
            set(&obj, "keys", arr.into());
        }
        MessageBody::ApiKeyCreateRequestBody(req) => {
            set(&obj, "variant", "ApiKeyCreateRequest".into());
            set(&obj, "name", req.name.into());
            set(&obj, "keyType", req.key_type.into());
            if let Some(sid) = req.subject_id {
                set(&obj, "subjectId", sid.into());
            }
            let scopes_arr = js_sys::Array::new();
            for r in req.scope_resources {
                let item = js_sys::Object::new();
                set(&item, "resourceType", r.resource_type.into());
                set(&item, "resourceId", r.resource_id.into());
                scopes_arr.push(&item.into());
            }
            set(&obj, "scopeResources", scopes_arr.into());
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
        MessageBody::ApiKeyScopeListResponse { entries } => {
            set(&obj, "variant", "ApiKeyScopeListResponse".into());
            let arr = js_sys::Array::new();
            for e in entries {
                let item = js_sys::Object::new();
                set(&item, "resourceType", e.resource_type.into());
                set(&item, "resourceId", e.resource_id.into());
                set(&item, "subjectType", e.subject_type.into());
                set(&item, "subjectId", e.subject_id.into());
                set(&item, "accessLevel", e.access_level.into());
                arr.push(&item.into());
            }
            set(&obj, "entries", arr.into());
        }
        MessageBody::ApiKeyRotateResponse { token } => {
            set(&obj, "variant", "ApiKeyRotateResponse".into());
            set(&obj, "token", token.into());
        }
        MessageBody::ApiKeyScopeListRequest { key_uid } => {
            set(&obj, "variant", "ApiKeyScopeListRequest".into());
            set(&obj, "keyUid", key_uid.into());
        }
        MessageBody::ApiKeyScopeSetRequest {
            key_uid,
            resource_type,
            resource_id,
            access_level,
        } => {
            set(&obj, "variant", "ApiKeyScopeSetRequest".into());
            set(&obj, "keyUid", key_uid.into());
            set(&obj, "resourceType", resource_type.into());
            set(&obj, "resourceId", resource_id.into());
            set(&obj, "accessLevel", access_level.into());
        }
        MessageBody::ApiKeyScopeClearRequest {
            key_uid,
            resource_type,
            resource_id,
        } => {
            set(&obj, "variant", "ApiKeyScopeClearRequest".into());
            set(&obj, "keyUid", key_uid.into());
            set(&obj, "resourceType", resource_type.into());
            set(&obj, "resourceId", resource_id.into());
        }
        MessageBody::ApiKeyRotateRequest { key_uid } => {
            set(&obj, "variant", "ApiKeyRotateRequest".into());
            set(&obj, "keyUid", key_uid.into());
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
            set(
                &obj,
                "userId",
                js_sys::Uint8Array::from(&resp.user_id[..]).into(),
            );
            set(&obj, "role", resp.role.into());
        }
        MessageBody::AuthMeRequest => {
            set(&obj, "variant", "AuthMeRequest".into());
        }
        MessageBody::AuthMeResponseBody(resp) => {
            set(&obj, "variant", "AuthMeResponse".into());
            set(
                &obj,
                "userId",
                js_sys::Uint8Array::from(&resp.user_id[..]).into(),
            );
            set(&obj, "username", resp.username.into());
            set(&obj, "role", resp.role.into());
        }
        MessageBody::MePreferencesGetRequestBody(_) => {
            set(&obj, "variant", "MePreferencesGetRequest".into());
        }
        MessageBody::MePreferencesGetResponseBody(resp) => {
            set(&obj, "variant", "MePreferencesGetResponse".into());
            match resp.language {
                Some(s) => set(&obj, "language", s.into()),
                None => set(&obj, "language", JsValue::NULL),
            }
        }
        MessageBody::MePreferencesUpdateRequestBody(req) => {
            set(&obj, "variant", "MePreferencesUpdateRequest".into());
            match req.language {
                Some(s) => set(&obj, "language", s.into()),
                None => set(&obj, "language", JsValue::NULL),
            }
        }
        MessageBody::MePreferencesUpdateResponseBody(resp) => {
            set(&obj, "variant", "MePreferencesUpdateResponse".into());
            match resp.language {
                Some(s) => set(&obj, "language", s.into()),
                None => set(&obj, "language", JsValue::NULL),
            }
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
            match req.flow_id {
                Some(f) => set(&obj, "flowId", f.into()),
                None => set(&obj, "flowId", JsValue::NULL),
            }
        }
        MessageBody::ChatStreamChunkBody(chunk) => {
            set(&obj, "variant", "ChatStreamChunk".into());
            set(&obj, "delta", chunk.delta.into());
        }
        MessageBody::ChatStreamEndBody(end) => {
            set(&obj, "variant", "ChatStreamEnd".into());
            set(&obj, "promptTokens", (end.prompt_tokens as u32).into());
            set(
                &obj,
                "completionTokens",
                (end.completion_tokens as u32).into(),
            );
            match end.text {
                Some(t) => set(&obj, "text", t.into()),
                None => set(&obj, "text", JsValue::NULL),
            }
            // Metryki wydajnosci jako Number (f64) — patrz memory: u64.into()
            // daloby BigInt i psulo arytmetyke w JS.
            set(&obj, "ttftMs", (end.ttft_ms as f64).into());
            set(&obj, "ttft_ms", (end.ttft_ms as f64).into());
            set(&obj, "prefillTps", (end.prefill_tps as f64).into());
            set(&obj, "prefill_tps", (end.prefill_tps as f64).into());
            set(&obj, "decodeTps", (end.decode_tps as f64).into());
            set(&obj, "decode_tps", (end.decode_tps as f64).into());
            set(&obj, "totalMs", (end.total_ms as f64).into());
            set(&obj, "total_ms", (end.total_ms as f64).into());
        }
        MessageBody::FlowInvokeRequestBody(_) => {
            // Serwer nie odsyła requestu do klienta; arm dla wyczerpalności.
            set(&obj, "variant", "FlowInvokeRequest".into());
        }
        MessageBody::FlowInvokeChunkBody(chunk) => {
            set(&obj, "variant", "FlowInvokeChunk".into());
            match chunk {
                tentaflow_protocol::FlowInvokeChunk::Text {
                    choice_index,
                    delta,
                } => {
                    set(&obj, "kind", "text".into());
                    set(&obj, "choiceIndex", choice_index.into());
                    set(&obj, "delta", delta.into());
                }
                tentaflow_protocol::FlowInvokeChunk::Audio {
                    choice_index,
                    mime,
                    sample_rate,
                    bytes,
                } => {
                    set(&obj, "kind", "audio".into());
                    set(&obj, "choiceIndex", choice_index.into());
                    set(&obj, "mime", mime.into());
                    if let Some(sr) = sample_rate {
                        set(&obj, "sampleRate", sr.into());
                    }
                    set(&obj, "bytes", js_sys::Uint8Array::from(&bytes[..]).into());
                }
                tentaflow_protocol::FlowInvokeChunk::Image { mime, bytes } => {
                    set(&obj, "kind", "image".into());
                    set(&obj, "mime", mime.into());
                    set(&obj, "bytes", js_sys::Uint8Array::from(&bytes[..]).into());
                }
                tentaflow_protocol::FlowInvokeChunk::Video { mime, bytes } => {
                    set(&obj, "kind", "video".into());
                    set(&obj, "mime", mime.into());
                    set(&obj, "bytes", js_sys::Uint8Array::from(&bytes[..]).into());
                }
                tentaflow_protocol::FlowInvokeChunk::File {
                    mime,
                    filename,
                    bytes,
                } => {
                    set(&obj, "kind", "file".into());
                    set(&obj, "mime", mime.into());
                    if let Some(fname) = filename {
                        set(&obj, "filename", fname.into());
                    }
                    set(&obj, "bytes", js_sys::Uint8Array::from(&bytes[..]).into());
                }
            }
        }
        MessageBody::FlowInvokeEndBody(end) => {
            set(&obj, "variant", "FlowInvokeEnd".into());
            set(&obj, "finishReason", end.finish_reason.into());
            if let Some(err) = end.error {
                set(&obj, "error", err.into());
            }
            if let Some(t) = end.text {
                set(&obj, "text", t.into());
            }
        }
        MessageBody::TranslateBody(tentaflow_protocol::TranslatePayload::Req(req)) => {
            set(&obj, "variant", "TranslateRequest".into());
            set(&obj, "sourceText", req.source_text.into());
            set(&obj, "sourceLang", req.source_lang.into());
            set(&obj, "targetLang", req.target_lang.into());
            if let Some(tone) = req.tone {
                set(&obj, "tone", tone.into());
            }
        }
        MessageBody::TranslateBody(tentaflow_protocol::TranslatePayload::Res(resp)) => {
            set(&obj, "variant", "TranslateResponse".into());
            set(&obj, "translatedText", resp.translated_text.into());
            if let Some(d) = resp.detected_source_lang {
                set(&obj, "detectedSourceLang", d.into());
            }
            set(&obj, "modelUsed", resp.model_used.into());
            set(&obj, "tokensUsed", resp.tokens_used.into());
        }
        MessageBody::ClusterUpdateRequestBody(req) => {
            set(&obj, "variant", "ClusterUpdateRequest".into());
            set(&obj, "clusterId", req.cluster_id.into());
            if let Some(n) = req.name {
                set(&obj, "name", n.into());
            }
            if let Some(d) = req.description {
                set(&obj, "description", d.into());
            }
            if let Some(s) = req.strategy {
                set(&obj, "strategy", s.into());
            }
            if let Some(b) = req.failover_enabled {
                set(&obj, "failoverEnabled", b.into());
            }
            if let Some(t) = req.failover_target {
                set(&obj, "failoverTarget", t.into());
            }
            if let Some(v) = req.health_check_interval_ms {
                set(&obj, "healthCheckIntervalMs", v.into());
            }
            if let Some(v) = req.timeout_ms {
                set(&obj, "timeoutMs", v.into());
            }
        }
        MessageBody::ClusterUpdateResponseBody(resp) => {
            set(&obj, "variant", "ClusterUpdateResponse".into());
            set(&obj, "ok", resp.ok.into());
        }
        MessageBody::MeshTrustEventBody(payload) => match payload {
            tentaflow_protocol::MeshTrustEventPayload::Revoked(evt) => {
                set(&obj, "variant", "MeshTrustRevoked".into());
                set(
                    &obj,
                    "revokedNodeId",
                    js_sys::Uint8Array::from(&evt.revoked_node_id[..]).into(),
                );
                set(&obj, "reason", evt.reason.into());
                set(&obj, "revokedAtEpoch", evt.revoked_at_epoch.into());
            }
            tentaflow_protocol::MeshTrustEventPayload::KeysSync(evt) => {
                set(&obj, "variant", "MeshTrustedKeysSync".into());
                let arr = js_sys::Array::new();
                for k in evt.trusted_keys {
                    arr.push(&js_sys::Uint8Array::from(&k[..]).into());
                }
                set(&obj, "trustedKeys", arr.into());
                set(&obj, "epoch", (evt.epoch as u32).into());
            }
        },
        MessageBody::SubscribeResumeRequest { resume_token } => {
            set(&obj, "variant", "SubscribeResumeRequest".into());
            set(
                &obj,
                "resumeToken",
                js_sys::Uint8Array::from(&resume_token[..]).into(),
            );
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
            set(
                &obj,
                "resumeToken",
                js_sys::Uint8Array::from(&resume_token[..]).into(),
            );
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
                set(&item, "isDefault", f.is_default.into());
                set(&item, "is_default", f.is_default.into());
                if let Some(pmn) = f.published_model_name {
                    set(&item, "publishedModelName", pmn.clone().into());
                    set(&item, "published_model_name", pmn.into());
                }
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
            if let Some(p) = req.published_model_name {
                set(&obj, "publishedModelName", p.into());
            }
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
            // `Some(Some(name))` republishes, `Some(None)` un-publishes,
            // `None` leaves the field untouched. Surface the distinction so
            // JS callers can tell "no change" from "explicit clear".
            if let Some(p) = r.published_model_name {
                set(&obj, "publishSet", true.into());
                if let Some(name) = p {
                    set(&obj, "publishedModelName", name.into());
                }
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
            set(
                &obj,
                "version",
                flow_version_full_to_js(resp.version).into(),
            );
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
                set(&item, "id", p.id.clone().into());
                set(&item, "name", p.name.into());
                set(&item, "providerType", p.provider_type.into());
                set(&item, "discoveryUrl", p.discovery_url.into());
                set(&item, "enabled", p.enabled.into());
                set(&item, "autoCreateUsers", p.auto_create_users.into());
                if let Some(g) = p.default_group_id {
                    set(&item, "defaultGroupId", g.clone().into());
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
                set(&obj, "defaultGroupId", g.clone().into());
            }
        }
        MessageBody::SsoProviderCreateResponseBody(resp) => {
            set(&obj, "variant", "SsoProviderCreateResponse".into());
            set(&obj, "id", resp.id.clone().into());
            set(&obj, "name", resp.name.into());
            set(&obj, "providerType", resp.provider_type.into());
        }
        MessageBody::SsoProviderDeleteRequestBody(req) => {
            set(&obj, "variant", "SsoProviderDeleteRequest".into());
            set(&obj, "id", req.id.clone().into());
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
                    set(&item, "minGpuMemoryGb", mem.clone().into());
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
                    a.declared_permissions_count.clone().into(),
                );
                set(
                    &item,
                    "usersWithOauthCount",
                    a.users_with_oauth_count.clone().into(),
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
                set(&item, "fileSizeBytes", a.file_size_bytes.clone().into());
                set(&item, "packageId", a.package_id.into());
                set(&item, "packageVersion", a.package_version.into());
                set(&item, "displayName", a.display_name.into());
                set(&item, "updateAvailable", a.update_available.into());
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
                    set(&obj, "userId", user_id.clone().into());
                }
                IP::ResGetUser { user } => {
                    set(&obj, "variant", "IamGetUserResponse".into());
                    set(&obj, "user", user_info_to_js(&user).into());
                }
                IP::ReqCreateUser { .. } => set(&obj, "variant", "IamCreateUserRequest".into()),
                IP::ResCreateUser { user_id } => {
                    set(&obj, "variant", "IamCreateUserResponse".into());
                    set(&obj, "userId", user_id.clone().into());
                }
                IP::ReqUpdateUser { .. } => set(&obj, "variant", "IamUpdateUserRequest".into()),
                IP::ReqDeleteUser { .. } => set(&obj, "variant", "IamDeleteUserRequest".into()),
                IP::ReqSetUserGroups { .. } => {
                    set(&obj, "variant", "IamSetUserGroupsRequest".into())
                }
                IP::ReqResetUserPassword { .. } => {
                    set(&obj, "variant", "IamResetUserPasswordRequest".into())
                }
                IP::ReqListGroups => set(&obj, "variant", "IamListGroupsRequest".into()),
                IP::ResListGroups { groups } => {
                    set(&obj, "variant", "IamListGroupsResponse".into());
                    let arr = js_sys::Array::new();
                    for g in groups {
                        let item = js_sys::Object::new();
                        set(&item, "id", g.id.clone().into());
                        set(&item, "name", g.name.clone().into());
                        set(&item, "description", g.description.clone().into());
                        set(&item, "memberCount", g.member_count.clone().into());
                        set(&item, "member_count", g.member_count.clone().into());
                        arr.push(&item.into());
                    }
                    set(&obj, "groups", arr.into());
                }
                IP::ReqCreateGroup { .. } => set(&obj, "variant", "IamCreateGroupRequest".into()),
                IP::ResCreateGroup { group_id } => {
                    set(&obj, "variant", "IamCreateGroupResponse".into());
                    set(&obj, "groupId", group_id.clone().into());
                }
                IP::ReqUpdateGroup { .. } => set(&obj, "variant", "IamUpdateGroupRequest".into()),
                IP::ReqDeleteGroup { .. } => set(&obj, "variant", "IamDeleteGroupRequest".into()),
                IP::ReqGroupMembers { .. } => set(&obj, "variant", "IamGroupMembersRequest".into()),
                IP::ResGroupMembers { members } => {
                    set(&obj, "variant", "IamGroupMembersResponse".into());
                    let arr = js_sys::Array::new();
                    for u in members.iter() {
                        arr.push(&user_info_to_js(u).into());
                    }
                    set(&obj, "members", arr.into());
                }
                IP::ReqSetPermission { .. } => {
                    set(&obj, "variant", "IamSetPermissionRequest".into())
                }
                IP::ReqClearPermission { .. } => {
                    set(&obj, "variant", "IamClearPermissionRequest".into())
                }
                IP::ReqListPermsForResource { .. } => {
                    set(&obj, "variant", "IamListPermsForResourceRequest".into())
                }
                IP::ReqListPermsForSubject { .. } => {
                    set(&obj, "variant", "IamListPermsForSubjectRequest".into())
                }
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
                        set(&item, "subjectId", e.subject_id.clone().into());
                        set(&item, "subject_id", e.subject_id.clone().into());
                        set(&item, "accessLevel", e.access_level.clone().into());
                        set(&item, "access_level", e.access_level.clone().into());
                        arr.push(&item.into());
                    }
                    set(&obj, "entries", arr.into());
                }
                IP::ResOk => set(&obj, "variant", "IamOkResponse".into()),
            }
        }

        // ---- Apps menu + UI v2 (schema v14) ----
        MessageBody::AddonUiBody(p) => {
            use tentaflow_protocol::AddonUiPayload as AP;
            match p {
                AP::ReqApplicationsList => {
                    set(&obj, "variant", "AddonApplicationsListRequest".into());
                }
                AP::ResApplicationsList { applications } => {
                    set(&obj, "variant", "AddonApplicationsListResponse".into());
                    let arr = js_sys::Array::new();
                    for a in applications {
                        let item = js_sys::Object::new();
                        set(&item, "addonId", a.addon_id.clone().into());
                        set(&item, "addon_id", a.addon_id.into());
                        set(&item, "title", a.title.into());
                        set(&item, "entryPanel", a.entry_panel.clone().into());
                        set(&item, "entry_panel", a.entry_panel.into());
                        set(&item, "icon", a.icon.into());
                        set(&item, "description", a.description.into());
                        set(&item, "sortOrder", a.sort_order.clone().into());
                        set(&item, "sort_order", a.sort_order.clone().into());
                        set(&item, "enabled", a.enabled.into());
                        arr.push(&item.into());
                    }
                    set(&obj, "applications", arr.into());
                }
            }
        }

        // ---- Multi-instance: katalog pakietow + operacje na instancjach ----
        MessageBody::AddonInstanceBody(p) => {
            use tentaflow_protocol::AddonInstancePayload as AP;
            match p {
                AP::ReqCatalogList => {
                    set(&obj, "variant", "AddonCatalogListRequest".into());
                }
                AP::ResCatalogList { packages } => {
                    set(&obj, "variant", "AddonCatalogListResponse".into());
                    let arr = js_sys::Array::new();
                    for pkg in packages {
                        let item = js_sys::Object::new();
                        set(&item, "packageId", pkg.package_id.clone().into());
                        set(&item, "package_id", pkg.package_id.into());
                        set(&item, "name", pkg.name.into());
                        set(&item, "latestVersion", pkg.latest_version.clone().into());
                        set(&item, "latest_version", pkg.latest_version.into());
                        let vers = js_sys::Array::new();
                        for v in pkg.versions {
                            vers.push(&JsValue::from(v));
                        }
                        set(&item, "versions", vers.into());
                        set(&item, "source", pkg.source.into());
                        set(&item, "installedInstances", pkg.installed_instances.into());
                        set(&item, "installed_instances", pkg.installed_instances.into());
                        let params = js_sys::Array::new();
                        for p in pkg.connection_params {
                            let pv = js_sys::Object::new();
                            set(&pv, "key", p.key.into());
                            set(&pv, "label", p.label.into());
                            set(&pv, "paramType", p.param_type.clone().into());
                            set(&pv, "param_type", p.param_type.into());
                            set(&pv, "required", p.required.into());
                            set(&pv, "placeholder", p.placeholder.into());
                            params.push(&pv.into());
                        }
                        set(&item, "connectionParams", params.clone().into());
                        set(&item, "connection_params", params.into());
                        arr.push(&item.into());
                    }
                    set(&obj, "packages", arr.into());
                }
                AP::ReqInstall(_) => {
                    set(&obj, "variant", "AddonInstanceInstallRequest".into());
                }
                AP::ResInstall(r) => {
                    set(&obj, "variant", "AddonInstanceInstallResponse".into());
                    set(&obj, "ok", r.ok.into());
                    if let Some(id) = r.addon_id {
                        set(&obj, "addonId", id.clone().into());
                        set(&obj, "addon_id", id.into());
                    }
                    if let Some(e) = r.error {
                        set(&obj, "error", e.into());
                    }
                }
                AP::ReqDuplicate(_) => {
                    set(&obj, "variant", "AddonInstanceDuplicateRequest".into());
                }
                AP::ReqVersions(_) => {
                    set(&obj, "variant", "AddonInstanceVersionsRequest".into());
                }
                AP::ResVersions(r) => {
                    set(&obj, "variant", "AddonInstanceVersionsResponse".into());
                    set(&obj, "current", r.current.into());
                    let arr = js_sys::Array::new();
                    for v in r.available {
                        arr.push(&JsValue::from(v));
                    }
                    set(&obj, "available", arr.into());
                }
                AP::ReqUpdate(_) => {
                    set(&obj, "variant", "AddonInstanceUpdateRequest".into());
                }
                AP::ResUpdate(r) => {
                    set(&obj, "variant", "AddonInstanceUpdateResponse".into());
                    set(&obj, "ok", r.ok.into());
                    if let Some(e) = r.error {
                        set(&obj, "error", e.into());
                    }
                }
            }
        }

        // ---- Storage stats addona (KV/SQL/Vector/Recording) ----
        MessageBody::AddonStorageBody(p) => {
            use tentaflow_protocol::AddonStoragePayload as SP;
            match p {
                SP::StatsRequest(_) => {
                    set(&obj, "variant", "AddonStorageStatsRequest".into());
                }
                SP::StatsResponse(r) => {
                    set(&obj, "variant", "AddonStorageStatsResponse".into());
                    // i64 -> f64 zeby JS dostal Number (nie BigInt); -1 = nieznane.
                    let kv = js_sys::Object::new();
                    set(&kv, "keys", (r.kv.keys as f64).into());
                    set(&kv, "bytes", (r.kv.bytes as f64).into());
                    set(&kv, "limitMb", (r.kv.limit_mb as f64).into());
                    set(&kv, "limit_mb", (r.kv.limit_mb as f64).into());
                    set(&obj, "kv", kv.into());

                    let sql = js_sys::Object::new();
                    set(&sql, "enabled", r.sql.enabled.into());
                    set(&sql, "available", r.sql.available.into());
                    set(&sql, "dbSizeBytes", (r.sql.db_size_bytes as f64).into());
                    set(&sql, "db_size_bytes", (r.sql.db_size_bytes as f64).into());
                    let tables = js_sys::Array::new();
                    for t in r.sql.tables {
                        let item = js_sys::Object::new();
                        set(&item, "name", t.name.into());
                        set(&item, "rows", (t.rows as f64).into());
                        set(&item, "rowsCapped", t.rows_capped.into());
                        set(&item, "rows_capped", t.rows_capped.into());
                        tables.push(&item.into());
                    }
                    set(&sql, "tables", tables.into());
                    set(&obj, "sql", sql.into());

                    let vector = js_sys::Object::new();
                    set(&vector, "available", r.vector.available.into());
                    let ns = js_sys::Array::new();
                    for n in r.vector.namespaces {
                        let item = js_sys::Object::new();
                        set(&item, "namespace", n.namespace.into());
                        set(&item, "dim", (n.dim as f64).into());
                        set(&item, "metric", n.metric.into());
                        set(&item, "count", (n.count as f64).into());
                        ns.push(&item.into());
                    }
                    set(&vector, "namespaces", ns.into());
                    set(&obj, "vector", vector.into());

                    let rec = js_sys::Object::new();
                    set(&rec, "available", r.recording.available.into());
                    set(&rec, "segments", (r.recording.segments as f64).into());
                    set(&rec, "snapshots", (r.recording.snapshots as f64).into());
                    set(&rec, "bytes", (r.recording.bytes as f64).into());
                    set(&obj, "recording", rec.into());
                }
            }
        }

        // ---- Vector backend picker addona ----
        MessageBody::AddonVectorBody(p) => {
            use tentaflow_protocol::AddonVectorPayload as VP;
            match p {
                VP::GetConfigRequest(_) => {
                    set(&obj, "variant", "AddonVectorGetConfigRequest".into());
                }
                VP::GetConfigResponse(r) => {
                    set(&obj, "variant", "AddonVectorGetConfigResponse".into());
                    set(&obj, "milvusCompiled", r.milvus_compiled.into());
                    set(&obj, "hasMilvusUser", r.has_milvus_user.into());
                    set(&obj, "hasMilvusPassword", r.has_milvus_password.into());
                    set(&obj, "config", vector_config_to_js(&r.config));
                    let arr = js_sys::Array::new();
                    for s in r.milvus_services {
                        let item = js_sys::Object::new();
                        set(&item, "nodeId", s.node_id.into());
                        set(&item, "local", s.local.into());
                        set(&item, "serviceId", s.service_id.into());
                        set(&item, "displayName", s.display_name.into());
                        set(&item, "endpoint", s.endpoint.into());
                        set(&item, "reachable", s.reachable.into());
                        arr.push(&item.into());
                    }
                    set(&obj, "milvusServices", arr.into());
                }
                VP::SetConfigRequest(_) => {
                    set(&obj, "variant", "AddonVectorSetConfigRequest".into());
                }
                VP::SetConfigResponse(r) => {
                    set(&obj, "variant", "AddonVectorSetConfigResponse".into());
                    set(&obj, "ok", r.ok.into());
                    if let Some(e) = r.error {
                        set(&obj, "error", e.into());
                    }
                }
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
                set(&item, "id", e.id.clone().into());
                set(&item, "timestamp", e.timestamp.into());
                set(&item, "action", e.action.into());
                if let Some(uid) = e.user_id {
                    set(&item, "userId", uid.clone().into());
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
            set(&obj, "totalCount", resp.total_count.clone().into());
        }
        MessageBody::AuditLogExportRequestBody(_) => {
            set(&obj, "variant", "AuditLogExportRequest".into());
        }
        MessageBody::AuditLogExportResponseBody(resp) => {
            set(&obj, "variant", "AuditLogExportResponse".into());
            set(&obj, "csv", resp.csv.into());
            set(&obj, "rowCount", resp.row_count.clone().into());
        }
        MessageBody::AuditLogCleanupRequestBody(req) => {
            set(&obj, "variant", "AuditLogCleanupRequest".into());
            set(&obj, "keepDays", req.keep_days.clone().into());
        }
        MessageBody::AuditLogCleanupResponseBody(resp) => {
            set(&obj, "variant", "AuditLogCleanupResponse".into());
            set(&obj, "deletedCount", resp.deleted_count.clone().into());
        }
        MessageBody::SchedulerBody(payload) => match payload {
            tentaflow_protocol::SchedulerPayload::JobsListRequest(_) => {
                set(&obj, "variant", "SchedulerJobsListRequest".into());
            }
            tentaflow_protocol::SchedulerPayload::JobsListResponse(resp) => {
                set(&obj, "variant", "SchedulerJobsListResponse".into());
                set(&obj, "jobsJson", resp.jobs_json.clone().into());
                set(&obj, "jobs_json", resp.jobs_json.into());
            }
            tentaflow_protocol::SchedulerPayload::ActionsListRequest(_) => {
                set(&obj, "variant", "SchedulerActionsListRequest".into());
            }
            tentaflow_protocol::SchedulerPayload::ActionsListResponse(resp) => {
                set(&obj, "variant", "SchedulerActionsListResponse".into());
                set(&obj, "actionsJson", resp.actions_json.clone().into());
                set(&obj, "actions_json", resp.actions_json.into());
            }
            tentaflow_protocol::SchedulerPayload::RunsListRequest(req) => {
                set(&obj, "variant", "SchedulerRunsListRequest".into());
                set(&obj, "jobId", req.job_id.clone().into());
                set(&obj, "job_id", req.job_id.into());
                set(&obj, "limit", req.limit.clone().into());
            }
            tentaflow_protocol::SchedulerPayload::RunsListResponse(resp) => {
                set(&obj, "variant", "SchedulerRunsListResponse".into());
                set(&obj, "runsJson", resp.runs_json.clone().into());
                set(&obj, "runs_json", resp.runs_json.into());
            }
            tentaflow_protocol::SchedulerPayload::JobUpsertRequest(req) => {
                set(&obj, "variant", "SchedulerJobUpsertRequest".into());
                set(&obj, "jobJson", req.job_json.clone().into());
                set(&obj, "job_json", req.job_json.into());
            }
            tentaflow_protocol::SchedulerPayload::JobUpsertResponse(resp) => {
                set(&obj, "variant", "SchedulerJobUpsertResponse".into());
                set(&obj, "jobJson", resp.job_json.clone().into());
                set(&obj, "job_json", resp.job_json.into());
            }
            tentaflow_protocol::SchedulerPayload::JobDeleteRequest(req) => {
                set(&obj, "variant", "SchedulerJobDeleteRequest".into());
                set(&obj, "jobId", req.job_id.clone().into());
                set(&obj, "job_id", req.job_id.into());
            }
            tentaflow_protocol::SchedulerPayload::JobDeleteResponse(resp) => {
                set(&obj, "variant", "SchedulerJobDeleteResponse".into());
                set(&obj, "ok", resp.ok.into());
            }
            tentaflow_protocol::SchedulerPayload::JobRunNowRequest(req) => {
                set(&obj, "variant", "SchedulerJobRunNowRequest".into());
                set(&obj, "jobId", req.job_id.clone().into());
                set(&obj, "job_id", req.job_id.into());
            }
            tentaflow_protocol::SchedulerPayload::JobRunNowResponse(resp) => {
                set(&obj, "variant", "SchedulerJobRunNowResponse".into());
                set(&obj, "runJson", resp.run_json.clone().into());
                set(&obj, "run_json", resp.run_json.into());
            }
        },
        MessageBody::TokenUsageBody(payload) => match payload {
            // Warianty request są obsługiwane tylko po stronie Core — dashboard
            // ich nie dekoduje, więc mapujemy je na sam znacznik wariantu.
            tentaflow_protocol::TokenUsagePayload::UsageSummaryRequest { .. } => {
                set(&obj, "variant", "TokenUsageSummaryRequest".into());
            }
            tentaflow_protocol::TokenUsagePayload::ListQuotasRequest => {
                set(&obj, "variant", "TokenListQuotasRequest".into());
            }
            tentaflow_protocol::TokenUsagePayload::UpsertQuotaRequest { .. } => {
                set(&obj, "variant", "TokenUpsertQuotaRequest".into());
            }
            tentaflow_protocol::TokenUsagePayload::DeleteQuotaRequest { .. } => {
                set(&obj, "variant", "TokenDeleteQuotaRequest".into());
            }
            tentaflow_protocol::TokenUsagePayload::CoordinatorStatusRequest => {
                set(&obj, "variant", "TokenCoordinatorStatusRequest".into());
            }
            tentaflow_protocol::TokenUsagePayload::UsageSummaryResponse { rows } => {
                set(&obj, "variant", "TokenUsageSummaryResponse".into());
                let arr = js_sys::Array::new();
                for r in rows {
                    let item = js_sys::Object::new();
                    set(&item, "key", r.key.into());
                    // Liczniki tokenów jako JS Number (f64) — i64 trafiłby do JS
                    // jako BigInt i psuł arytmetykę/wykresy w dashboardzie.
                    set(&item, "promptTokens", (r.prompt_tokens as f64).into());
                    set(&item, "prompt_tokens", (r.prompt_tokens as f64).into());
                    set(&item, "completionTokens", (r.completion_tokens as f64).into());
                    set(&item, "completion_tokens", (r.completion_tokens as f64).into());
                    set(&item, "totalTokens", (r.total_tokens as f64).into());
                    set(&item, "total_tokens", (r.total_tokens as f64).into());
                    set(&item, "requestCount", (r.request_count as f64).into());
                    set(&item, "request_count", (r.request_count as f64).into());
                    set(&item, "audioMs", (r.audio_ms as f64).into());
                    set(&item, "audio_ms", (r.audio_ms as f64).into());
                    set(&item, "images", (r.images as f64).into());
                    set(&item, "embeddingTokens", (r.embedding_tokens as f64).into());
                    set(&item, "embedding_tokens", (r.embedding_tokens as f64).into());
                    arr.push(&item);
                }
                set(&obj, "rows", arr.into());
            }
            tentaflow_protocol::TokenUsagePayload::ListQuotasResponse { quotas } => {
                set(&obj, "variant", "TokenListQuotasResponse".into());
                let arr = js_sys::Array::new();
                for q in quotas {
                    let item = js_sys::Object::new();
                    set(&item, "id", q.id.into());
                    set(&item, "orgId", q.org_id.clone().into());
                    set(&item, "org_id", q.org_id.into());
                    set(&item, "scopeType", q.scope_type.clone().into());
                    set(&item, "scope_type", q.scope_type.into());
                    set_optional_string(&item, "subjectId", q.subject_id.clone());
                    set_optional_string(&item, "subject_id", q.subject_id);
                    set_optional_string(&item, "modelId", q.model_id.clone());
                    set_optional_string(&item, "model_id", q.model_id);
                    set(&item, "period", q.period.into());
                    set(&item, "maxTotalTokens", (q.max_total_tokens as f64).into());
                    set(&item, "max_total_tokens", (q.max_total_tokens as f64).into());
                    set(&item, "isActive", q.is_active.into());
                    set(&item, "is_active", q.is_active.into());
                    arr.push(&item);
                }
                set(&obj, "quotas", arr.into());
            }
            tentaflow_protocol::TokenUsagePayload::UpsertQuotaResponse { id } => {
                set(&obj, "variant", "TokenUpsertQuotaResponse".into());
                set(&obj, "id", id.into());
            }
            tentaflow_protocol::TokenUsagePayload::DeleteQuotaResponse => {
                set(&obj, "variant", "TokenDeleteQuotaResponse".into());
            }
            tentaflow_protocol::TokenUsagePayload::CoordinatorStatusResponse {
                coordinator_node_id,
                leases,
            } => {
                set(&obj, "variant", "TokenCoordinatorStatusResponse".into());
                set_optional_string(&obj, "coordinatorNodeId", coordinator_node_id.clone());
                set_optional_string(&obj, "coordinator_node_id", coordinator_node_id);
                let arr = js_sys::Array::new();
                for l in leases {
                    let item = js_sys::Object::new();
                    set(&item, "id", l.id.into());
                    set(&item, "quotaId", l.quota_id.clone().into());
                    set(&item, "quota_id", l.quota_id.into());
                    set(&item, "nodeId", l.node_id.clone().into());
                    set(&item, "node_id", l.node_id.into());
                    set(&item, "periodKey", l.period_key.clone().into());
                    set(&item, "period_key", l.period_key.into());
                    set(&item, "baseUsed", (l.base_used as f64).into());
                    set(&item, "base_used", (l.base_used as f64).into());
                    set(&item, "grantedTokens", (l.granted_tokens as f64).into());
                    set(&item, "granted_tokens", (l.granted_tokens as f64).into());
                    set(&item, "coordinatorNodeId", l.coordinator_node_id.clone().into());
                    set(&item, "coordinator_node_id", l.coordinator_node_id.into());
                    set(&item, "expiresAt", l.expires_at.clone().into());
                    set(&item, "expires_at", l.expires_at.into());
                    arr.push(&item);
                }
                set(&obj, "leases", arr.into());
            }
        },
        MessageBody::ModelMetricsBody(payload) => decode_model_metrics_payload(&obj, payload),
        MessageBody::BenchmarkBody(payload) => decode_benchmark_payload(&obj, payload),
        MessageBody::SkillsBody(payload) => match payload {
            tentaflow_protocol::SkillsPayload::ListRequest(req) => {
                set(&obj, "variant", "SkillsListRequest".into());
                set_optional_string(&obj, "tag", req.tag);
                set_optional_string(&obj, "source", req.source);
                set_optional_string(&obj, "status", req.status);
            }
            tentaflow_protocol::SkillsPayload::ListResponse(resp) => {
                set(&obj, "variant", "SkillsListResponse".into());
                set(&obj, "skillsJson", resp.skills_json.clone().into());
                set(&obj, "skills_json", resp.skills_json.into());
            }
            tentaflow_protocol::SkillsPayload::DetailRequest(req) => {
                set(&obj, "variant", "SkillsDetailRequest".into());
                set(&obj, "skillId", req.skill_id.clone().into());
                set(&obj, "skill_id", req.skill_id.into());
            }
            tentaflow_protocol::SkillsPayload::DetailResponse(resp) => {
                set(&obj, "variant", "SkillsDetailResponse".into());
                set(&obj, "skillJson", resp.skill_json.clone().into());
                set(&obj, "skill_json", resp.skill_json.into());
                set(&obj, "filesJson", resp.files_json.clone().into());
                set(&obj, "files_json", resp.files_json.into());
            }
            tentaflow_protocol::SkillsPayload::UpsertRequest(req) => {
                set(&obj, "variant", "SkillsUpsertRequest".into());
                set(&obj, "skillJson", req.skill_json.clone().into());
                set(&obj, "skill_json", req.skill_json.into());
            }
            tentaflow_protocol::SkillsPayload::UpsertResponse(resp) => {
                set(&obj, "variant", "SkillsUpsertResponse".into());
                set(&obj, "skillId", resp.skill_id.clone().into());
                set(&obj, "skill_id", resp.skill_id.into());
            }
            tentaflow_protocol::SkillsPayload::DeleteRequest(req) => {
                set(&obj, "variant", "SkillsDeleteRequest".into());
                set(&obj, "skillId", req.skill_id.clone().into());
                set(&obj, "skill_id", req.skill_id.into());
            }
            tentaflow_protocol::SkillsPayload::DeleteResponse(resp) => {
                set(&obj, "variant", "SkillsDeleteResponse".into());
                set(&obj, "deleted", resp.deleted.into());
            }
            tentaflow_protocol::SkillsPayload::ForkRequest(req) => {
                set(&obj, "variant", "SkillsForkRequest".into());
                set(&obj, "skillId", req.skill_id.clone().into());
                set(&obj, "skill_id", req.skill_id.into());
                set(&obj, "newName", req.new_name.clone().into());
                set(&obj, "new_name", req.new_name.into());
            }
            tentaflow_protocol::SkillsPayload::ForkResponse(resp) => {
                set(&obj, "variant", "SkillsForkResponse".into());
                set(&obj, "skillId", resp.skill_id.clone().into());
                set(&obj, "skill_id", resp.skill_id.into());
            }
            tentaflow_protocol::SkillsPayload::HubSearchRequest(req) => {
                set(&obj, "variant", "SkillsHubSearchRequest".into());
                set(&obj, "query", req.query.into());
                set_optional_string(&obj, "source", req.source);
            }
            tentaflow_protocol::SkillsPayload::HubSearchResponse(resp) => {
                set(&obj, "variant", "SkillsHubSearchResponse".into());
                set(&obj, "resultsJson", resp.results_json.clone().into());
                set(&obj, "results_json", resp.results_json.into());
            }
            tentaflow_protocol::SkillsPayload::HubImportRequest(req) => {
                set(&obj, "variant", "SkillsHubImportRequest".into());
                set(&obj, "source", req.source.into());
                set_optional_string(&obj, "gitRef", req.git_ref.clone());
                set_optional_string(&obj, "git_ref", req.git_ref);
            }
            tentaflow_protocol::SkillsPayload::HubImportResponse(resp) => {
                set(&obj, "variant", "SkillsHubImportResponse".into());
                set(&obj, "skillId", resp.skill_id.clone().into());
                set(&obj, "skill_id", resp.skill_id.into());
                set(&obj, "verdictJson", resp.verdict_json.clone().into());
                set(&obj, "verdict_json", resp.verdict_json.into());
            }
            tentaflow_protocol::SkillsPayload::HubApproveRequest(req) => {
                set(&obj, "variant", "SkillsHubApproveRequest".into());
                set(&obj, "skillId", req.skill_id.clone().into());
                set(&obj, "skill_id", req.skill_id.into());
            }
            tentaflow_protocol::SkillsPayload::HubApproveResponse(resp) => {
                set(&obj, "variant", "SkillsHubApproveResponse".into());
                set(&obj, "approved", resp.approved.into());
            }
            tentaflow_protocol::SkillsPayload::HubRejectRequest(req) => {
                set(&obj, "variant", "SkillsHubRejectRequest".into());
                set(&obj, "skillId", req.skill_id.clone().into());
                set(&obj, "skill_id", req.skill_id.into());
            }
            tentaflow_protocol::SkillsPayload::HubRejectResponse(resp) => {
                set(&obj, "variant", "SkillsHubRejectResponse".into());
                set(&obj, "rejected", resp.rejected.into());
            }
            tentaflow_protocol::SkillsPayload::CuratorRunRequest(_) => {
                set(&obj, "variant", "SkillsCuratorRunRequest".into());
            }
            tentaflow_protocol::SkillsPayload::CuratorRunResponse(resp) => {
                set(&obj, "variant", "SkillsCuratorRunResponse".into());
                set(&obj, "proposalJson", resp.proposal_json.clone().into());
                set(&obj, "proposal_json", resp.proposal_json.into());
                set(&obj, "snapshotId", resp.snapshot_id.clone().into());
                set(&obj, "snapshot_id", resp.snapshot_id.into());
            }
            tentaflow_protocol::SkillsPayload::CuratorApplyRequest(req) => {
                set(&obj, "variant", "SkillsCuratorApplyRequest".into());
                set(&obj, "snapshotId", req.snapshot_id.clone().into());
                set(&obj, "snapshot_id", req.snapshot_id.into());
                set(&obj, "approvedActionsJson", req.approved_actions_json.clone().into());
                set(&obj, "approved_actions_json", req.approved_actions_json.into());
            }
            tentaflow_protocol::SkillsPayload::CuratorApplyResponse(resp) => {
                set(&obj, "variant", "SkillsCuratorApplyResponse".into());
                set(&obj, "mutated", resp.mutated.into());
            }
            tentaflow_protocol::SkillsPayload::CuratorRollbackRequest(req) => {
                set(&obj, "variant", "SkillsCuratorRollbackRequest".into());
                set(&obj, "snapshotId", req.snapshot_id.clone().into());
                set(&obj, "snapshot_id", req.snapshot_id.into());
            }
            tentaflow_protocol::SkillsPayload::CuratorRollbackResponse(resp) => {
                set(&obj, "variant", "SkillsCuratorRollbackResponse".into());
                set(&obj, "restored", resp.restored.into());
            }
        },
        MessageBody::AgentsBody(payload) => match payload {
            tentaflow_protocol::AgentsPayload::ListRequest(req) => {
                set(&obj, "variant", "AgentsListRequest".into());
                set(
                    &obj,
                    "enabled",
                    req.enabled.map(JsValue::from).unwrap_or(JsValue::NULL),
                );
                set(
                    &obj,
                    "routable",
                    req.routable.map(JsValue::from).unwrap_or(JsValue::NULL),
                );
            }
            tentaflow_protocol::AgentsPayload::ListResponse(resp) => {
                set(&obj, "variant", "AgentsListResponse".into());
                set(&obj, "agentsJson", resp.agents_json.clone().into());
                set(&obj, "agents_json", resp.agents_json.into());
            }
            tentaflow_protocol::AgentsPayload::DetailRequest(req) => {
                set(&obj, "variant", "AgentsDetailRequest".into());
                set(&obj, "agentId", req.agent_id.clone().into());
                set(&obj, "agent_id", req.agent_id.into());
            }
            tentaflow_protocol::AgentsPayload::DetailResponse(resp) => {
                set(&obj, "variant", "AgentsDetailResponse".into());
                set(&obj, "agentJson", resp.agent_json.clone().into());
                set(&obj, "agent_json", resp.agent_json.into());
            }
            tentaflow_protocol::AgentsPayload::UpsertRequest(req) => {
                set(&obj, "variant", "AgentsUpsertRequest".into());
                set(&obj, "agentJson", req.agent_json.clone().into());
                set(&obj, "agent_json", req.agent_json.into());
            }
            tentaflow_protocol::AgentsPayload::UpsertResponse(resp) => {
                set(&obj, "variant", "AgentsUpsertResponse".into());
                set(&obj, "agentId", resp.agent_id.clone().into());
                set(&obj, "agent_id", resp.agent_id.into());
            }
            tentaflow_protocol::AgentsPayload::DeleteRequest(req) => {
                set(&obj, "variant", "AgentsDeleteRequest".into());
                set(&obj, "agentId", req.agent_id.clone().into());
                set(&obj, "agent_id", req.agent_id.into());
            }
            tentaflow_protocol::AgentsPayload::DeleteResponse(resp) => {
                set(&obj, "variant", "AgentsDeleteResponse".into());
                set(&obj, "deleted", resp.deleted.into());
            }
            tentaflow_protocol::AgentsPayload::RunsListRequest(req) => {
                set(&obj, "variant", "AgentRunsListRequest".into());
                set_optional_string(&obj, "agentId", req.agent_id.clone());
                set_optional_string(&obj, "agent_id", req.agent_id);
                set_optional_string(&obj, "status", req.status);
                set_optional_string(&obj, "parentRunId", req.parent_run_id.clone());
                set_optional_string(&obj, "parent_run_id", req.parent_run_id);
            }
            tentaflow_protocol::AgentsPayload::RunsListResponse(resp) => {
                set(&obj, "variant", "AgentRunsListResponse".into());
                set(&obj, "runsJson", resp.runs_json.clone().into());
                set(&obj, "runs_json", resp.runs_json.into());
            }
            tentaflow_protocol::AgentsPayload::RunDetailRequest(req) => {
                set(&obj, "variant", "AgentRunDetailRequest".into());
                set(&obj, "runId", req.run_id.clone().into());
                set(&obj, "run_id", req.run_id.into());
            }
            tentaflow_protocol::AgentsPayload::RunDetailResponse(resp) => {
                set(&obj, "variant", "AgentRunDetailResponse".into());
                set(&obj, "runJson", resp.run_json.clone().into());
                set(&obj, "run_json", resp.run_json.into());
            }
            tentaflow_protocol::AgentsPayload::ToolsCatalogRequest(_) => {
                set(&obj, "variant", "ToolsCatalogRequest".into());
            }
            tentaflow_protocol::AgentsPayload::ToolsCatalogResponse(resp) => {
                set(&obj, "variant", "ToolsCatalogResponse".into());
                set(&obj, "toolsJson", resp.tools_json.clone().into());
                set(&obj, "tools_json", resp.tools_json.into());
            }
            tentaflow_protocol::AgentsPayload::RunReplyRequest(req) => {
                set(&obj, "variant", "AgentRunReplyRequest".into());
                set(&obj, "runId", req.run_id.clone().into());
                set(&obj, "run_id", req.run_id.into());
                set(&obj, "questionId", req.question_id.clone().into());
                set(&obj, "question_id", req.question_id.into());
                set(&obj, "answer", req.answer.into());
            }
            tentaflow_protocol::AgentsPayload::RunReplyResponse(resp) => {
                set(&obj, "variant", "AgentRunReplyResponse".into());
                set(&obj, "delivered", resp.delivered.into());
            }
            tentaflow_protocol::AgentsPayload::PermissionReplyRequest(req) => {
                set(&obj, "variant", "AgentPermissionReplyRequest".into());
                set(&obj, "runId", req.run_id.clone().into());
                set(&obj, "run_id", req.run_id.into());
                set(&obj, "requestId", req.request_id.clone().into());
                set(&obj, "request_id", req.request_id.into());
                set(&obj, "decision", req.decision.into());
            }
            tentaflow_protocol::AgentsPayload::PermissionReplyResponse(resp) => {
                set(&obj, "variant", "AgentPermissionReplyResponse".into());
                set(&obj, "delivered", resp.delivered.into());
            }
            tentaflow_protocol::AgentsPayload::RunCancelRequest(req) => {
                set(&obj, "variant", "AgentRunCancelRequest".into());
                set(&obj, "runId", req.run_id.clone().into());
                set(&obj, "run_id", req.run_id.into());
            }
            tentaflow_protocol::AgentsPayload::RunCancelResponse(resp) => {
                set(&obj, "variant", "AgentRunCancelResponse".into());
                set(&obj, "cancelled", resp.cancelled.into());
            }
            tentaflow_protocol::AgentsPayload::RunEventsSubscribeRequest(req) => {
                set(&obj, "variant", "AgentRunEventsSubscribeRequest".into());
                let (kind, id) = match req.scope {
                    tentaflow_protocol::AgentRunEventScope::Session { session_id } => {
                        ("session", session_id)
                    }
                    tentaflow_protocol::AgentRunEventScope::Run { run_id } => ("run", run_id),
                };
                set(&obj, "scopeKind", kind.into());
                set(&obj, "scope_kind", kind.into());
                set(&obj, "scopeId", id.clone().into());
                set(&obj, "scope_id", id.into());
            }
            tentaflow_protocol::AgentsPayload::RunEvent(ev) => {
                set(&obj, "variant", "AgentRunEvent".into());
                set(&obj, "scope", ev.scope.into());
                set(&obj, "kind", ev.kind.into());
                set(&obj, "runId", ev.run_id.clone().into());
                set(&obj, "run_id", ev.run_id.into());
                set(&obj, "nodeId", ev.node_id.clone().into());
                set(&obj, "node_id", ev.node_id.into());
                set(&obj, "nodeType", ev.node_type.clone().into());
                set(&obj, "node_type", ev.node_type.into());
                set(&obj, "status", ev.status.into());
                set(&obj, "name", ev.name.into());
                set(&obj, "agent", ev.agent.into());
                set(&obj, "n", ev.n.into());
                set(&obj, "max", ev.max.into());
                set(&obj, "index", ev.index.into());
                set(&obj, "total", ev.total.into());
                set(&obj, "selected", ev.selected.into());
                set(&obj, "reason", ev.reason.into());
                set(&obj, "interactionId", ev.interaction_id.clone().into());
                set(&obj, "interaction_id", ev.interaction_id.into());
                set(&obj, "question", ev.question.into());
                let choices = js_sys::Array::new();
                for c in ev.choices {
                    choices.push(&JsValue::from_str(&c));
                }
                set(&obj, "choices", choices.into());
                set(&obj, "addonId", ev.addon_id.clone().into());
                set(&obj, "addon_id", ev.addon_id.into());
                set(&obj, "toolName", ev.tool_name.clone().into());
                set(&obj, "tool_name", ev.tool_name.into());
                set(&obj, "permission", ev.permission.into());
                set(&obj, "outcome", ev.outcome.into());
            }
        },
        MessageBody::SyncConflictBody(payload) => match payload {
            tentaflow_protocol::SyncConflictPayload::ListRequest(req) => {
                set(&obj, "variant", "SyncConflictsListRequest".into());
                set(&obj, "orgId", req.org_id.clone().into());
                set(&obj, "org_id", req.org_id.into());
                set(&obj, "addonId", req.addon_id.clone().into());
                set(&obj, "addon_id", req.addon_id.into());
                set(&obj, "status", req.status.into());
                set(&obj, "limit", req.limit.clone().into());
            }
            tentaflow_protocol::SyncConflictPayload::ListResponse(resp) => {
                set(&obj, "variant", "SyncConflictsListResponse".into());
                let arr = js_sys::Array::new();
                for conflict in resp.conflicts {
                    let item = js_sys::Object::new();
                    set(&item, "operationId", conflict.operation_id.clone().into());
                    set(&item, "operation_id", conflict.operation_id.into());
                    set(&item, "orgId", conflict.org_id.clone().into());
                    set(&item, "org_id", conflict.org_id.into());
                    set(&item, "addonId", conflict.addon_id.clone().into());
                    set(&item, "addon_id", conflict.addon_id.into());
                    set(&item, "tableName", conflict.table_name.clone().into());
                    set(&item, "table_name", conflict.table_name.into());
                    set(&item, "resourceType", conflict.resource_type.clone().into());
                    set(&item, "resource_type", conflict.resource_type.into());
                    set(&item, "resourceId", conflict.resource_id.clone().into());
                    set(&item, "resource_id", conflict.resource_id.into());
                    set(&item, "action", conflict.action.into());
                    set(
                        &item,
                        "sourceNodeId",
                        conflict.source_node_id.clone().into(),
                    );
                    set(&item, "source_node_id", conflict.source_node_id.into());
                    set(&item, "errorKind", conflict.error_kind.clone().into());
                    set(&item, "error_kind", conflict.error_kind.into());
                    set(&item, "errorMessage", conflict.error_message.clone().into());
                    set(&item, "error_message", conflict.error_message.into());
                    set(&item, "status", conflict.status.into());
                    set(&item, "createdAtMs", conflict.created_at_ms.clone().into());
                    set(
                        &item,
                        "created_at_ms",
                        conflict.created_at_ms.clone().into(),
                    );
                    set_optional_i64(&item, "resolvedAtMs", conflict.resolved_at_ms);
                    set_optional_i64(&item, "resolved_at_ms", conflict.resolved_at_ms);
                    set_optional_string(&item, "resolution", conflict.resolution);
                    arr.push(&item);
                }
                set(&obj, "conflicts", arr.into());
            }
            tentaflow_protocol::SyncConflictPayload::ResolveRequest(req) => {
                set(&obj, "variant", "SyncConflictResolveRequest".into());
                set(&obj, "orgId", req.org_id.clone().into());
                set(&obj, "org_id", req.org_id.into());
                set(&obj, "addonId", req.addon_id.clone().into());
                set(&obj, "addon_id", req.addon_id.into());
                set(&obj, "operationId", req.operation_id.clone().into());
                set(&obj, "operation_id", req.operation_id.into());
                set(
                    &obj,
                    "resolution",
                    sync_conflict_resolution_to_str(req.resolution).into(),
                );
            }
            tentaflow_protocol::SyncConflictPayload::ResolveResponse(resp) => {
                set(&obj, "variant", "SyncConflictResolveResponse".into());
                set(&obj, "operationId", resp.operation_id.clone().into());
                set(&obj, "operation_id", resp.operation_id.into());
                set(&obj, "status", resp.status.into());
                set(&obj, "resolution", resp.resolution.into());
                set(&obj, "rowsAffected", resp.rows_affected.clone().into());
                set(&obj, "rows_affected", resp.rows_affected.clone().into());
            }
        },
        MessageBody::SyncStorageBody(payload) => match payload {
            tentaflow_protocol::SyncStoragePayload::ReportRequest(_) => {
                set(&obj, "variant", "SyncStorageReportRequest".into());
            }
            tentaflow_protocol::SyncStoragePayload::ReportResponse(resp) => {
                set(&obj, "variant", "SyncStorageReportResponse".into());
                set(&obj, "root", resp.root.into());
                set(&obj, "level", sync_storage_level_to_str(resp.level).into());
                set_optional_u64(&obj, "totalBytes", resp.total_bytes);
                set_optional_u64(&obj, "total_bytes", resp.total_bytes);
                set_optional_u64(&obj, "availableBytes", resp.available_bytes);
                set_optional_u64(&obj, "available_bytes", resp.available_bytes);
                set_optional_u32(&obj, "freePercentBps", resp.free_percent_bps);
                set_optional_u32(&obj, "free_percent_bps", resp.free_percent_bps);
                set(&obj, "sqliteBytes", resp.sqlite_bytes.clone().into());
                set(&obj, "sqlite_bytes", resp.sqlite_bytes.clone().into());
                set(
                    &obj,
                    "fjallLedgerBytes",
                    resp.fjall_ledger_bytes.clone().into(),
                );
                set(
                    &obj,
                    "fjall_ledger_bytes",
                    resp.fjall_ledger_bytes.clone().into(),
                );
                set(
                    &obj,
                    "snapshotBlobBytes",
                    resp.snapshot_blob_bytes.clone().into(),
                );
                set(
                    &obj,
                    "snapshot_blob_bytes",
                    resp.snapshot_blob_bytes.clone().into(),
                );
                set(
                    &obj,
                    "finalBlobBytes",
                    resp.final_blob_bytes.clone().into(),
                );
                set(
                    &obj,
                    "final_blob_bytes",
                    resp.final_blob_bytes.clone().into(),
                );
                set(
                    &obj,
                    "pendingBlobChunkBytes",
                    resp.pending_blob_chunk_bytes.clone().into(),
                );
                set(
                    &obj,
                    "pending_blob_chunk_bytes",
                    resp.pending_blob_chunk_bytes.clone().into(),
                );
                set(
                    &obj,
                    "largeBlobBlockBytes",
                    resp.large_blob_block_bytes.clone().into(),
                );
                set(
                    &obj,
                    "large_blob_block_bytes",
                    resp.large_blob_block_bytes.clone().into(),
                );
                let arr = js_sys::Array::new();
                for path in resp.paths {
                    let item = js_sys::Object::new();
                    set(&item, "label", path.label.into());
                    set(&item, "path", path.path.into());
                    set(&item, "bytes", path.bytes.clone().into());
                    arr.push(&item);
                }
                set(&obj, "paths", arr.into());
            }
        },
        MessageBody::ServiceBody(payload) => decode_service_payload(&obj, payload),
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
                    set(&item, "id", n.id.clone().into());
                    set(&item, "title", n.title.into());
                    set(&item, "bodyPreview", n.body_preview.clone().into());
                    set(&item, "body_preview", n.body_preview.into());
                    set(&item, "pinned", n.pinned.into());
                    set(&item, "createdAtEpoch", n.created_at_epoch.clone().into());
                    set(
                        &item,
                        "created_at_epoch",
                        n.created_at_epoch.clone().into(),
                    );
                    set(&item, "updatedAtEpoch", n.updated_at_epoch.clone().into());
                    set(
                        &item,
                        "updated_at_epoch",
                        n.updated_at_epoch.clone().into(),
                    );
                    arr.push(&item.into());
                }
                set(&obj, "notes", arr.into());
            }
            NotesResponse::Detail(d) => {
                set(&obj, "variant", "NoteDetailResponse".into());
                set(&obj, "id", d.id.clone().into());
                set(&obj, "title", d.title.into());
                set(&obj, "body", d.body.into());
                set(&obj, "pinned", d.pinned.into());
                set(&obj, "createdAtEpoch", d.created_at_epoch.clone().into());
                set(&obj, "created_at_epoch", d.created_at_epoch.clone().into());
                set(&obj, "updatedAtEpoch", d.updated_at_epoch.clone().into());
                set(&obj, "updated_at_epoch", d.updated_at_epoch.clone().into());
            }
            NotesResponse::Create(c) => {
                set(&obj, "variant", "NoteCreateResponse".into());
                set(&obj, "id", c.id.clone().into());
            }
            NotesResponse::Update(u) => {
                set(&obj, "variant", "NoteUpdateResponse".into());
                set(&obj, "ok", u.ok.into());
                set(&obj, "updatedAtEpoch", u.updated_at_epoch.clone().into());
                set(&obj, "updated_at_epoch", u.updated_at_epoch.clone().into());
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
            set(&obj, "tsEpoch", (e.ts_epoch as f64).into());
            if let Some(u) = e.user_id {
                set(&obj, "userId", js_sys::Uint8Array::from(&u[..]).into());
            }
            set(&obj, "eventKind", e.event_kind.into());
            if let Some(r) = e.resource_id {
                set(&obj, "resourceId", r.into());
            }
            set(&obj, "message", e.message.into());
        }
        MessageBody::ContainerBody(payload) => match payload {
            tentaflow_protocol::ContainerPayload::ListRequest => {
                set(&obj, "variant", "ContainerListRequest".into());
            }
            tentaflow_protocol::ContainerPayload::ListResponse { containers } => {
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
            tentaflow_protocol::ContainerPayload::StartRequest { container_id } => {
                set(&obj, "variant", "ContainerStartRequest".into());
                set(&obj, "containerId", container_id.into());
            }
            tentaflow_protocol::ContainerPayload::StartResponse { started } => {
                set(&obj, "variant", "ContainerStartResponse".into());
                set(&obj, "started", started.into());
            }
            tentaflow_protocol::ContainerPayload::StopRequest { container_id } => {
                set(&obj, "variant", "ContainerStopRequest".into());
                set(&obj, "containerId", container_id.into());
            }
            tentaflow_protocol::ContainerPayload::StopResponse { stopped } => {
                set(&obj, "variant", "ContainerStopResponse".into());
                set(&obj, "stopped", stopped.into());
            }
            tentaflow_protocol::ContainerPayload::LogStreamRequest {
                container_id,
                follow,
            } => {
                set(&obj, "variant", "ContainerLogStreamRequest".into());
                set(&obj, "containerId", container_id.into());
                set(&obj, "follow", follow.into());
            }
            tentaflow_protocol::ContainerPayload::LogChunkBody(c) => {
                set(&obj, "variant", "ContainerLogChunk".into());
                set(&obj, "containerId", c.container_id.into());
                set(&obj, "stream", c.stream.into());
                set(&obj, "line", c.line.into());
                set(&obj, "tsEpoch", (c.ts_epoch as f64).into());
            }
        },
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
        MessageBody::TtsPreviewRequest { text, model, voice } => {
            set(&obj, "variant", "TtsPreviewRequest".into());
            set(&obj, "text", text.into());
            set(&obj, "model", model.into());
            set(&obj, "voice", voice.into());
        }
        MessageBody::TtsPreviewResponse { bytes, format } => {
            set(&obj, "variant", "TtsPreviewResponse".into());
            set(&obj, "bytes", js_sys::Uint8Array::from(&bytes[..]).into());
            set(&obj, "format", format.into());
        }
        MessageBody::PiiRuleBody(p) => match p {
            tentaflow_protocol::PiiRulePayload::ListRequest => {
                set(&obj, "variant", "PiiRuleListRequest".into());
            }
            tentaflow_protocol::PiiRulePayload::ListResponse { rules } => {
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
        },
        MessageBody::VisionBody(p) => match p {
            tentaflow_protocol::VisionInferPayload::InferRequest(_) => {
                set(&obj, "variant", "VisionInferRequest".into());
            }
            tentaflow_protocol::VisionInferPayload::InferResponse(r) => {
                set(&obj, "variant", "VisionInferResponse".into());
                set(&obj, "serviceName", r.service_name.into());
                set(&obj, "latencyMs", r.latency_ms.clone().into());
                match r.result {
                    tentaflow_protocol::VisionInferResult::Faces(faces) => {
                        set(&obj, "kind", "faces".into());
                        let arr = js_sys::Array::new();
                        for f in faces {
                            let item = js_sys::Object::new();
                            set(&item, "x1", f.x1.into());
                            set(&item, "y1", f.y1.into());
                            set(&item, "x2", f.x2.into());
                            set(&item, "y2", f.y2.into());
                            set(&item, "score", f.score.into());
                            let kp_arr = js_sys::Array::new();
                            for (x, y) in f.keypoints {
                                let pt = js_sys::Array::new();
                                pt.push(&x.into());
                                pt.push(&y.into());
                                kp_arr.push(&pt.into());
                            }
                            set(&item, "keypoints", kp_arr.into());
                            arr.push(&item.into());
                        }
                        set(&obj, "faces", arr.into());
                    }
                    tentaflow_protocol::VisionInferResult::AgeGender {
                        age_years,
                        gender_male_prob,
                    } => {
                        set(&obj, "kind", "age_gender".into());
                        set(&obj, "ageYears", age_years.into());
                        set(&obj, "genderMaleProb", gender_male_prob.into());
                    }
                    tentaflow_protocol::VisionInferResult::Emotion {
                        label,
                        probabilities,
                        valence,
                        arousal,
                    } => {
                        set(&obj, "kind", "emotion".into());
                        set(&obj, "label", label.into());
                        let arr = js_sys::Array::new();
                        for (k, v) in probabilities {
                            let pair = js_sys::Array::new();
                            pair.push(&k.into());
                            pair.push(&v.into());
                            arr.push(&pair.into());
                        }
                        set(&obj, "probabilities", arr.into());
                        if let Some(v) = valence {
                            set(&obj, "valence", v.into());
                        }
                        if let Some(a) = arousal {
                            set(&obj, "arousal", a.into());
                        }
                    }
                    tentaflow_protocol::VisionInferResult::Poses(poses) => {
                        // Pose detection result (added with the vision pose
                        // models). Surface keypoints and bbox to JS as an
                        // array; downstream UI does the drawing.
                        set(&obj, "kind", "poses".into());
                        let arr = js_sys::Array::new();
                        for p in poses {
                            let item = js_sys::Object::new();
                            set(&item, "x1", p.x1.into());
                            set(&item, "y1", p.y1.into());
                            set(&item, "x2", p.x2.into());
                            set(&item, "y2", p.y2.into());
                            set(&item, "score", p.score.into());
                            let kp_arr = js_sys::Array::new();
                            for kp in p.keypoints {
                                let kp_item = js_sys::Object::new();
                                set(&kp_item, "id", (kp.id as u32).into());
                                set(&kp_item, "name", kp.name.into());
                                set(&kp_item, "x", kp.x.into());
                                set(&kp_item, "y", kp.y.into());
                                set(&kp_item, "score", kp.score.into());
                                kp_arr.push(&kp_item.into());
                            }
                            set(&item, "keypoints", kp_arr.into());
                            arr.push(&item.into());
                        }
                        set(&obj, "poses", arr.into());
                    }
                }
            }
        },
        MessageBody::RerankBody(p) => match p {
            tentaflow_protocol::RerankExchange::Request(_) => {
                set(&obj, "variant", "RerankRequest".into());
            }
            tentaflow_protocol::RerankExchange::Response(r) => {
                set(&obj, "variant", "RerankResponse".into());
                set(&obj, "model", r.model.into());
                let arr = js_sys::Array::new();
                for item in r.results {
                    let entry = js_sys::Object::new();
                    set(&entry, "index", (item.index as u32).into());
                    set(&entry, "relevanceScore", item.relevance_score.into());
                    if let Some(doc) = item.document {
                        set(&entry, "document", doc.into());
                    }
                    arr.push(&entry.into());
                }
                set(&obj, "results", arr.into());
            }
        },
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
                set(
                    &item,
                    "nodeId",
                    js_sys::Uint8Array::from(&p.node_id[..]).into(),
                );
                set(&item, "displayName", p.display_name.into());
                set(&item, "trustState", p.trust_state.into());
                if let Some(ep) = p.endpoint {
                    set(&item, "endpoint", ep.into());
                }
                if let Some(ls) = p.last_seen_epoch {
                    set(&item, "lastSeenEpoch", (ls as f64).into());
                }
                arr.push(&item.into());
            }
            set(&obj, "peers", arr.into());
        }
        MessageBody::MeshPairInitRequestBody(req) => {
            set(&obj, "variant", "MeshPairInitRequest".into());
            set(
                &obj,
                "nodeId",
                js_sys::Uint8Array::from(&req.node_id[..]).into(),
            );
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
            set(&obj, "cpuUsagePercent", s.cpu_usage_percent.clone().into());
            set(&obj, "ramUsedMb", (s.ram_used_mb as f64).into());
            set(&obj, "ramTotalMb", (s.ram_total_mb as f64).into());
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
                arr.push(&cluster_member_to_js(m).into());
            }
            set(&obj, "members", arr.into());
        }
        MessageBody::ClusterCreateRequestBody(req) => {
            set(&obj, "variant", "ClusterCreateRequest".into());
            set(&obj, "name", req.name.into());
            if let Some(d) = req.description {
                set(&obj, "description", d.into());
            }
            set(&obj, "strategy", req.strategy.into());
            set(&obj, "failoverEnabled", req.failover_enabled.into());
            if let Some(t) = req.failover_target {
                set(&obj, "failoverTarget", t.into());
            }
            set(
                &obj,
                "healthCheckIntervalMs",
                req.health_check_interval_ms.into(),
            );
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
            if let Some(t) = req.interface_type {
                set(&obj, "interfaceType", t.into());
            }
            if let Some(s) = req.interface_speed_mbps {
                set(&obj, "interfaceSpeedMbps", s.into());
            }
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
            for n in req.node_ids {
                arr.push(&n.into());
            }
            set(&obj, "nodeIds", arr.into());
        }
        MessageBody::ClusterProbeStreamChunkBody(c) => {
            set(&obj, "variant", "ClusterProbeStreamChunk".into());
            set(&obj, "eventType", c.event_type.into());
            if let Some(s) = c.source_node {
                set(&obj, "sourceNode", s.into());
            }
            if let Some(t) = c.target_node {
                set(&obj, "targetNode", t.into());
            }
            if let Some(s) = c.success {
                set(&obj, "success", s.into());
            }
            if let Some(v) = c.latency_ms {
                set(&obj, "latencyMs", v.into());
            }
            if let Some(v) = c.bandwidth_mbps {
                set(&obj, "bandwidthMbps", v.into());
            }
            if let Some(t) = c.interface_type {
                set(&obj, "interfaceType", t.into());
            }
            if let Some(m) = c.message {
                set(&obj, "message", m.into());
            }
        }
        MessageBody::ClusterProbeStreamEndBody(e) => {
            set(&obj, "variant", "ClusterProbeStreamEnd".into());
            set(&obj, "totalPairs", e.total_pairs.into());
            set(&obj, "successful", e.successful.into());
            set(&obj, "failed", e.failed.into());
            if let Some(b) = e.bottleneck_mbps {
                set(&obj, "bottleneckMbps", b.into());
            }
            if let Some(s) = e.assignment_status {
                set(&obj, "assignmentStatus", s.into());
            }
            let arr = js_sys::Array::new();
            for a in e.assignments {
                let item = js_sys::Object::new();
                set(&item, "nodeId", a.node_id.into());
                set(&item, "interfaceName", a.interface_name.into());
                set(&item, "interfaceIp", a.interface_ip.into());
                set(&item, "interfaceSpeedMbps", a.interface_speed_mbps.into());
                set(&item, "interfaceType", a.interface_type.into());
                arr.push(&item.into());
            }
            set(&obj, "assignments", arr.into());
        }
        MessageBody::ClusterRdmaConfigureRequestBody(req) => {
            set(&obj, "variant", "ClusterRdmaConfigureRequest".into());
            set(&obj, "clusterId", req.cluster_id.into());
            // sudo_password intentionally NOT surfaced back to JS.
            if let Some(m) = req.mtu {
                set(&obj, "mtu", m.into());
            }
        }
        MessageBody::ClusterRdmaConfigureResponseBody(resp) => {
            set(&obj, "variant", "ClusterRdmaConfigureResponse".into());
            set(&obj, "ok", resp.ok.into());
            if let Some(msg) = resp.message {
                set(&obj, "message", msg.into());
            }
            let members = js_sys::Array::new();
            for m in resp.members {
                let item = js_sys::Object::new();
                set(&item, "nodeId", m.node_id.into());
                set(&item, "hostname", m.hostname.into());
                set(&item, "status", m.status.into());
                if let Some(err) = m.error {
                    set(&item, "error", err.into());
                }
                let ifaces = js_sys::Array::new();
                for i in m.interfaces {
                    let io = js_sys::Object::new();
                    set(&io, "netdev", i.netdev.into());
                    set(&io, "roceDevice", i.roce_device.into());
                    if let Some(ip) = i.ipv4 {
                        set(&io, "ipv4", ip.into());
                    }
                    set(&io, "mtu", i.mtu.into());
                    set(&io, "role", i.role.into());
                    set(&io, "action", i.action.into());
                    ifaces.push(&io.into());
                }
                set(&item, "interfaces", ifaces.into());
                members.push(&item.into());
            }
            set(&obj, "members", members.into());
        }
        // ---- Cluster distributed deploy (D3) ----
        MessageBody::ClusterDeployRequestBody(req) => {
            set(&obj, "variant", "ClusterDeployRequest".into());
            set(&obj, "clusterId", req.cluster_id.into());
            set(&obj, "engineId", req.engine_id.into());
        }
        MessageBody::ClusterDeployResponseBody(resp) => {
            set(&obj, "variant", "ClusterDeployResponse".into());
            set(&obj, "ok", resp.ok.into());
            set(&obj, "deploymentClusterId", resp.deployment_cluster_id.into());
            set(&obj, "headNodeId", resp.head_node_id.into());
            if let Some(url) = resp.endpoint_url {
                set(&obj, "endpointUrl", url.into());
            }
            if let Some(msg) = resp.message {
                set(&obj, "message", msg.into());
            }
            let members = js_sys::Array::new();
            for m in resp.members {
                let item = js_sys::Object::new();
                set(&item, "nodeId", m.node_id.into());
                set(&item, "hostname", m.hostname.into());
                set(&item, "role", m.role.into());
                set(&item, "ok", m.ok.into());
                if let Some(id) = m.deploy_id {
                    set(&item, "deployId", id.into());
                }
                if let Some(err) = m.error {
                    set(&item, "error", err.into());
                }
                members.push(&item.into());
            }
            set(&obj, "members", members.into());
        }
        MessageBody::ClusterDeployStopRequestBody(req) => {
            set(&obj, "variant", "ClusterDeployStopRequest".into());
            set(&obj, "clusterId", req.cluster_id.into());
            set(&obj, "deploymentClusterId", req.deployment_cluster_id.into());
        }
        MessageBody::ClusterDeployStopResponseBody(resp) => {
            set(&obj, "variant", "ClusterDeployStopResponse".into());
            set(&obj, "ok", resp.ok.into());
            if let Some(msg) = resp.message {
                set(&obj, "message", msg.into());
            }
            let members = js_sys::Array::new();
            for m in resp.members {
                let item = js_sys::Object::new();
                set(&item, "nodeId", m.node_id.into());
                set(&item, "hostname", m.hostname.into());
                set(&item, "role", m.role.into());
                set(&item, "ok", m.ok.into());
                if let Some(err) = m.error {
                    set(&item, "error", err.into());
                }
                members.push(&item.into());
            }
            set(&obj, "members", members.into());
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
                set(&item, "initiatedAt", p.initiated_at.clone().into());
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
            set(
                &obj,
                "invitePinExpiresSec",
                resp.invite_pin_expires_sec.clone().into(),
            );
            set(
                &obj,
                "invite_pin_expires_sec",
                resp.invite_pin_expires_sec.clone().into(),
            );
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
                set(
                    &item,
                    "trustedSinceEpoch",
                    t.trusted_since_epoch.clone().into(),
                );
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
        MessageBody::CatalogListRequestBody(r) => {
            set(&obj, "variant", "CatalogListRequest".into());
            if let Some(ref s) = r.surface_filter {
                set(&obj, "surfaceFilter", s.clone().into());
            }
            set(
                &obj,
                "includeBlockingDiagnostics",
                r.include_blocking_diagnostics.into(),
            );
        }
        MessageBody::CatalogListResponseBody(resp) => {
            set(&obj, "variant", "CatalogListResponse".into());
            set(&obj, "version", resp.version.clone().into());
            let arr = js_sys::Array::new();
            for entry in resp.entries {
                let item = js_sys::Object::new();
                set(&item, "id", entry.id.clone().into());
                set(&item, "ownedBy", entry.owned_by.into());
                set(
                    &item,
                    "serviceSurfaces",
                    string_vec_to_js(entry.service_surfaces).into(),
                );
                set(
                    &item,
                    "inputModalities",
                    string_vec_to_js(entry.input_modalities).into(),
                );
                set(
                    &item,
                    "outputModalities",
                    string_vec_to_js(entry.output_modalities).into(),
                );

                let kind = js_sys::Object::new();
                match entry.kind {
                    tentaflow_protocol::CatalogEntryKindWire::ServiceModel { instances } => {
                        set(&kind, "kind", "service_model".into());
                        let inst_arr = js_sys::Array::new();
                        for i in instances {
                            let inst = js_sys::Object::new();
                            set(&inst, "nodeId", i.node_id.clone().into());
                            if let Some(ref h) = i.node_hostname {
                                set(&inst, "nodeHostname", h.clone().into());
                            }
                            set(&inst, "serviceId", i.service_id.clone().into());
                            set(&inst, "status", i.status.into());
                            if let Some(b) = i.backend {
                                set(&inst, "backend", b.into());
                            }
                            if let Some(s) = i.size_mb {
                                set(&inst, "sizeMb", s.clone().into());
                            }
                            set(&inst, "loaded", i.loaded.into());
                            inst_arr.push(&inst.into());
                        }
                        set(&kind, "instances", inst_arr.into());
                    }
                    tentaflow_protocol::CatalogEntryKindWire::Flow {
                        flow_id,
                        published_name,
                    } => {
                        set(&kind, "kind", "flow".into());
                        set(&kind, "flowId", flow_id.clone().into());
                        set(&kind, "publishedName", published_name.into());
                    }
                    tentaflow_protocol::CatalogEntryKindWire::Alias {
                        target,
                        fallback_targets,
                        strategy,
                    } => {
                        set(&kind, "kind", "alias".into());
                        set(&kind, "target", target.into());
                        set(
                            &kind,
                            "fallbackTargets",
                            string_vec_to_js(fallback_targets).into(),
                        );
                        set(&kind, "strategy", strategy.into());
                    }
                }
                set(&item, "kind", kind.into());

                if let Some(diag) = entry.diagnostic {
                    let d = js_sys::Object::new();
                    match diag {
                        tentaflow_protocol::CatalogDiagnosticWire::RemoteShadowed {
                            local_owner,
                        } => {
                            set(&d, "kind", "remote_shadowed".into());
                            set(&d, "localOwner", local_owner.into());
                        }
                        tentaflow_protocol::CatalogDiagnosticWire::LocalOverride {
                            conflicting_remote_node,
                        } => {
                            set(&d, "kind", "local_override".into());
                            set(&d, "conflictingRemoteNode", conflicting_remote_node.into());
                        }
                        tentaflow_protocol::CatalogDiagnosticWire::IncompatibleAliasTargets {
                            alias,
                            missing_modalities,
                        } => {
                            set(&d, "kind", "incompatible_alias_targets".into());
                            set(&d, "alias", alias.into());
                            set(
                                &d,
                                "missingModalities",
                                string_vec_to_js(missing_modalities).into(),
                            );
                        }
                    }
                    set(&item, "diagnostic", d.into());
                }
                arr.push(&item.into());
            }
            set(&obj, "entries", arr.into());
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
            if let Some(s) = r.strategy {
                set(&obj, "strategy", s.into());
            }
            if let Some(f) = r.fallback_targets {
                set(&obj, "fallbackTargets", f.clone().into());
                set(&obj, "fallback_targets", f.into());
            }
        }
        MessageBody::ModelAliasCreateResponseBody(r) => {
            set(&obj, "variant", "ModelAliasCreateResponse".into());
            set(&obj, "id", r.id.clone().into());
        }
        MessageBody::ModelAliasUpdateRequestBody(r) => {
            set(&obj, "variant", "ModelAliasUpdateRequest".into());
            set(&obj, "id", r.id.clone().into());
            set(&obj, "alias", r.alias.into());
            set(&obj, "targetModel", r.target_model.clone().into());
            set(&obj, "target_model", r.target_model.into());
            if let Some(a) = r.is_active {
                set(&obj, "isActive", a.into());
                set(&obj, "is_active", a.into());
            }
            if let Some(s) = r.strategy {
                set(&obj, "strategy", s.into());
            }
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
            set(&obj, "id", r.id.clone().into());
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
            set(&obj, "fileSizeBytes", resp.file_size_bytes.clone().into());
            set(
                &obj,
                "file_size_bytes",
                resp.file_size_bytes.clone().into(),
            );
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
            set(
                &obj,
                "visibilityGroupsVisible",
                resp.visibility_groups_visible.clone().into(),
            );
            set(
                &obj,
                "visibility_groups_visible",
                resp.visibility_groups_visible.clone().into(),
            );
            set(
                &obj,
                "visibilityGroupsTotal",
                resp.visibility_groups_total.clone().into(),
            );
            set(
                &obj,
                "visibility_groups_total",
                resp.visibility_groups_total.clone().into(),
            );
            set(&obj, "toolsCount", resp.tools_count.clone().into());
            set(&obj, "tools_count", resp.tools_count.clone().into());
            set(
                &obj,
                "linkedAccountsCount",
                resp.linked_accounts_count.clone().into(),
            );
            set(
                &obj,
                "linked_accounts_count",
                resp.linked_accounts_count.clone().into(),
            );
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
                set(&item, "groupId", r.group_id.clone().into());
                set(&item, "group_id", r.group_id.clone().into());
                set(&item, "groupName", r.group_name.clone().into());
                set(&item, "group_name", r.group_name.into());
                set(&item, "visible", r.visible.into());
                set(
                    &item,
                    "groupDescription",
                    r.group_description.clone().into(),
                );
                set(&item, "group_description", r.group_description.into());
                set(&item, "userCount", r.user_count.clone().into());
                set(&item, "user_count", r.user_count.clone().into());
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
            set(&obj, "groupId", req.group_id.clone().into());
            set(&obj, "group_id", req.group_id.clone().into());
            set(&obj, "visible", req.visible.into());
        }
        MessageBody::AddonVisibilitySetResponseBody(resp) => {
            set(&obj, "variant", "AddonVisibilitySetResponse".into());
            set(&obj, "addonId", resp.addon_id.clone().into());
            set(&obj, "addon_id", resp.addon_id.into());
            set(&obj, "groupId", resp.group_id.clone().into());
            set(&obj, "group_id", resp.group_id.clone().into());
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
            set(
                &obj,
                "lastChangeAtEpoch",
                resp.last_change_at_epoch.clone().into(),
            );
            set(
                &obj,
                "last_change_at_epoch",
                resp.last_change_at_epoch.clone().into(),
            );
        }
        MessageBody::AddonPermissionSetRequestBody(req) => {
            set(&obj, "variant", "AddonPermissionSetRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
            set(&obj, "subjectType", req.subject_type.clone().into());
            set(&obj, "subject_type", req.subject_type.into());
            set(&obj, "subjectId", req.subject_id.clone().into());
            set(&obj, "subject_id", req.subject_id.clone().into());
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
            set(&obj, "subjectId", resp.subject_id.clone().into());
            set(&obj, "subject_id", resp.subject_id.clone().into());
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
                set(&obj, "userId", uid.clone().into());
                set(&obj, "user_id", uid.clone().into());
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
            set(
                &obj,
                "variant",
                "AddonOAuthConfigClearSecretResponse".into(),
            );
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
            set(&obj, "accountId", req.account_id.clone().into());
            set(&obj, "account_id", req.account_id.clone().into());
        }
        MessageBody::AddonOAuthRevokeResponseBody(resp) => {
            set(&obj, "variant", "AddonOAuthRevokeResponse".into());
            set(&obj, "accountId", resp.account_id.clone().into());
            set(&obj, "account_id", resp.account_id.clone().into());
            set(&obj, "revoked", resp.revoked.into());
        }
        MessageBody::AddonOAuthReauthorizeRequestBody(req) => {
            set(&obj, "variant", "AddonOAuthReauthorizeRequest".into());
            set(&obj, "accountId", req.account_id.clone().into());
            set(&obj, "account_id", req.account_id.clone().into());
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
        MessageBody::SystemEventBody(evt) => match evt {
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
        },
        MessageBody::AddonPermissionChangedEventBody(evt) => {
            set(&obj, "variant", "AddonPermissionChangedEvent".into());
            set(&obj, "addonId", evt.addon_id.clone().into());
            set(&obj, "addon_id", evt.addon_id.into());
            if let Some(st) = evt.subject_type {
                set(&obj, "subjectType", st.clone().into());
                set(&obj, "subject_type", st.into());
            }
            if let Some(sid) = evt.subject_id {
                set(&obj, "subjectId", sid.clone().into());
                set(&obj, "subject_id", sid.clone().into());
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
            set(&obj, "limit", r.limit.clone().into());
            set(&obj, "offset", r.offset.clone().into());
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
                set(&eo, "id", e.id.clone().into());
                set(&eo, "timestamp", e.timestamp.into());
                set(&eo, "level", e.level.into());
                set(&eo, "action", e.action.into());
                set(&eo, "message", e.message.into());
                if let Some(uid) = e.user_id {
                    set(&eo, "userId", uid.clone().into());
                }
                if let Some(un) = e.user_name {
                    set(&eo, "userName", un.into());
                }
                set(&eo, "details", e.details.into());
                arr.push(&eo.into());
            }
            set(&obj, "entries", arr.into());
            set(&obj, "total", r.total.clone().into());
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
            set(&obj, "maxInstances", r.max_instances.clone().into());
            set(&obj, "cpuLimitPct", r.cpu_limit_pct.clone().into());
            set(&obj, "ramMb", r.ram_mb.clone().into());
            set(&obj, "storageMb", r.storage_mb.clone().into());
            set(
                &obj,
                "httpRequestsPerMin",
                r.http_requests_per_min.clone().into(),
            );
            set(
                &obj,
                "llmTokensPerMin",
                r.llm_tokens_per_min.clone().into(),
            );
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
                set(&item, "ruleId", d.rule_id.clone().into());
                set(&item, "rule_id", d.rule_id.into());
                set(&item, "host", d.host.into());
                match d.port {
                    Some(p) => set(&item, "port", p.clone().into()),
                    None => set(&item, "port", JsValue::NULL),
                }
                set(&item, "protocol", d.protocol.into());
                set(&item, "mode", d.mode.into());
                set(&item, "status", d.status.into());
                set(&item, "required", d.required.into());
                set(&item, "approved", d.approved.into());
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
                set(&obj, "sessionId", r.session_id.clone().into());
                set(&obj, "session_id", r.session_id.clone().into());
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
            set(&obj, "timestampMs", event.timestamp_ms.clone().into());
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
                    set(&obj, "rttMs", info.rtt_ms.clone().into());
                    set(&obj, "rtt_ms", info.rtt_ms.clone().into());
                    set(
                        &obj,
                        "lastCheckUnixSecs",
                        info.last_check_unix_secs.clone().into(),
                    );
                    set(
                        &obj,
                        "last_check_unix_secs",
                        info.last_check_unix_secs.clone().into(),
                    );
                    set(
                        &obj,
                        "lastSuccessUnixSecs",
                        info.last_success_unix_secs.clone().into(),
                    );
                    set(
                        &obj,
                        "last_success_unix_secs",
                        info.last_success_unix_secs.clone().into(),
                    );
                    set(&obj, "status", info.status.clone().into());
                    set(&obj, "bindAddrActual", info.bind_addr_actual.clone().into());
                    set(
                        &obj,
                        "bind_addr_actual",
                        info.bind_addr_actual.clone().into(),
                    );
                }
            }
        }
        MessageBody::ProfilingBody(payload) => {
            profiling_payload_fill_obj(&obj, &payload);
        }
        MessageBody::DeployVllmRecommendRequestBody(_) => {
            // Request nigdy nie wraca do GUI jako odpowiedz — wystarczy variant tag.
            set(&obj, "variant", "DeployVllmRecommendRequest".into());
        }
        MessageBody::DeployVllmRecommendResponseBody(payload) => {
            set(&obj, "variant", "DeployVllmRecommendResponse".into());
            // Cala odpowiedz ma 60+ pol w 4 zagniezdzonych structach — zamiast
            // recznie kopiowac kazdy field, serializujemy do JSON i zwracamy
            // jako pojedynczy string. GUI robi JSON.parse() na polu `json`.
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            set(&obj, "json", json.into());
        }
        MessageBody::SuggestServicePortRequestBody(_) => {
            set(&obj, "variant", "SuggestServicePortRequest".into());
        }
        MessageBody::SuggestServicePortResponseBody(payload) => {
            set(&obj, "variant", "SuggestServicePortResponse".into());
            set(&obj, "port", payload.port.into());
            set(&obj, "available", payload.available.into());
        }
        MessageBody::EngineRecommendRequestBody(_) => {
            set(&obj, "variant", "EngineRecommendRequest".into());
        }
        MessageBody::EngineRecommendResponseBody(payload) => {
            set(&obj, "variant", "EngineRecommendResponse".into());
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            set(&obj, "json", json.into());
        }
        MessageBody::CameraAdminBody(payload) => match payload {
            tentaflow_protocol::CameraAdminPayload::DiscoverRequest(_) => {
                set(&obj, "variant", "CameraDiscoverRequest".into());
            }
            tentaflow_protocol::CameraAdminPayload::DiscoverResponse(resp) => {
                set(&obj, "variant", "CameraDiscoverResponse".into());
                let arr = js_sys::Array::new();
                for cam in resp.discovered {
                    let item = js_sys::Object::new();
                    // Map wire field names to the dashboard wizard schema:
                    // `vendor` (manufacturer), `model`, `ip` (address), plus
                    // raw `xaddrs` + `types` for advanced flows.
                    set(&item, "vendor", cam.manufacturer.clone().into());
                    set(&item, "manufacturer", cam.manufacturer.into());
                    set(&item, "model", cam.model.into());
                    set(&item, "ip", cam.address.clone().into());
                    set(&item, "address", cam.address.into());
                    let xaddrs = js_sys::Array::new();
                    for x in cam.xaddrs {
                        xaddrs.push(&JsValue::from_str(&x));
                    }
                    set(&item, "xaddrs", xaddrs.into());
                    let types = js_sys::Array::new();
                    for t in cam.types {
                        types.push(&JsValue::from_str(&t));
                    }
                    set(&item, "types", types.into());
                    arr.push(&item.into());
                }
                set(&obj, "discovered", arr.into());
            }
            tentaflow_protocol::CameraAdminPayload::AddOnvifRequest(_) => {
                // Request variants never legitimately decode in a response
                // path. The server emits *Response variants; this branch only
                // exists for exhaustiveness. Surface the variant name only —
                // no credentials, username, or device URL — so a stray
                // request body in a debug buffer cannot leak admin secrets
                // to the JS layer.
                set(&obj, "variant", "CameraAddOnvifRequest".into());
                set(
                    &obj,
                    "warning",
                    "unexpected_request_variant_in_response".into(),
                );
            }
            tentaflow_protocol::CameraAdminPayload::AddOnvifResponse(resp) => {
                set(&obj, "variant", "CameraAddOnvifResponse".into());
                set(&obj, "cameraId", resp.camera_id.clone().into());
                set(&obj, "camera_id", resp.camera_id.into());
                set(&obj, "rtspUrl", resp.rtsp_url.clone().into());
                set(&obj, "rtsp_url", resp.rtsp_url.into());
                set(&obj, "profileToken", resp.profile_token.clone().into());
                set(&obj, "profile_token", resp.profile_token.into());
            }
            tentaflow_protocol::CameraAdminPayload::FrameUrlRequest(_) => {
                // Defense-in-depth: a stray request body must never echo the
                // camera_id or ttl back to the JS layer. Surface the variant
                // tag only — same pattern as AddOnvifRequest above.
                set(&obj, "variant", "CameraFrameUrlRequest".into());
                set(
                    &obj,
                    "warning",
                    "unexpected_request_variant_in_response".into(),
                );
            }
            tentaflow_protocol::CameraAdminPayload::FrameUrlResponse(resp) => {
                set(&obj, "variant", "CameraFrameUrlResponse".into());
                set(&obj, "signedUrl", resp.signed_url.clone().into());
                set(&obj, "signed_url", resp.signed_url.into());
                set(&obj, "expiresAtMs", resp.expires_at_ms.clone().into());
                set(&obj, "expires_at_ms", resp.expires_at_ms.clone().into());
            }
            tentaflow_protocol::CameraAdminPayload::DetectionsSubscribeRequest(_) => {
                // Request variant never legitimately arrives in a response/chunk
                // path. Surface the variant tag only — same defense-in-depth
                // pattern as the other CameraAdminPayload request branches.
                set(&obj, "variant", "CameraDetectionsSubscribeRequest".into());
                set(
                    &obj,
                    "warning",
                    "unexpected_request_variant_in_response".into(),
                );
            }
            tentaflow_protocol::CameraAdminPayload::DetectionsFrame(frame) => {
                set(&obj, "variant", "CameraDetectionsFrame".into());
                set(&obj, "cameraId", frame.camera_id.clone().into());
                set(&obj, "camera_id", frame.camera_id.into());
                set(&obj, "tsMs", (frame.ts_ms as f64).into());
                set(&obj, "ts_ms", (frame.ts_ms as f64).into());
                // PTS klatki w osi czasu mediów (ns) — wspólny zegar z init
                // segmentem MSE, kotwiczy overlay na dokładnej klatce wideo.
                set_optional_u64(&obj, "pts_ns", frame.pts_ns);
                set_optional_u64(&obj, "ptsNs", frame.pts_ns);
                // Całkowity czas obróbki klatki (ms): detekcja + OCR + stan.
                // Klient renderuje jako badge opóźnienia.
                set(&obj, "proc_ms", (frame.proc_ms as f64).into());
                set(&obj, "procMs", (frame.proc_ms as f64).into());
                let items = js_sys::Array::new();
                for det in frame.items {
                    let item = js_sys::Object::new();
                    set(&item, "klasa", det.klasa.into());
                    let bbox = js_sys::Array::new();
                    for v in det.bbox {
                        bbox.push(&JsValue::from_f64(v as f64));
                    }
                    set(&item, "bbox", bbox.into());
                    set(&item, "score", (det.score as f64).into());
                    let stan = js_sys::Array::new();
                    for s in det.stan {
                        stan.push(&JsValue::from_str(&s));
                    }
                    set(&item, "stan", stan.into());
                    match det.tekst {
                        Some(t) => set(&item, "tekst", t.into()),
                        None => set(&item, "tekst", JsValue::NULL),
                    }
                    // Stabilne id trackingu oraz prędkość środka boksu
                    // (jednostki znormalizowane/s) dla ekstrapolacji overlayu.
                    set(&item, "track_id", (det.track_id as f64).into());
                    set(&item, "vx", (det.vx as f64).into());
                    set(&item, "vy", (det.vy as f64).into());
                    items.push(&item.into());
                }
                set(&obj, "items", items.into());
            }
        },
        MessageBody::LegalAdminBody(payload) => match payload {
            tentaflow_protocol::LegalAdminPayload::ListRequest(_) => {
                // Request variants never legitimately arrive in a response
                // path. Surface variant tag only — defense-in-depth mirror of
                // CameraAdminPayload request-in-response handling.
                set(&obj, "variant", "LegalDocumentsListRequest".into());
                set(
                    &obj,
                    "warning",
                    "unexpected_request_variant_in_response".into(),
                );
            }
            tentaflow_protocol::LegalAdminPayload::ListResponse(resp) => {
                set(&obj, "variant", "LegalDocumentsListResponse".into());
                let arr = js_sys::Array::new();
                for doc in resp.documents {
                    let item = js_sys::Object::new();
                    set(&item, "doc_id", doc.doc_id.clone().into());
                    set(&item, "docId", doc.doc_id.into());
                    set(&item, "org_id", doc.org_id.clone().into());
                    set(&item, "orgId", doc.org_id.into());
                    set(&item, "variant", doc.variant.into());
                    // `generated_at` on the wire is unix-ms — expose under the
                    // dashboard's preferred `generated_at_ms` key plus camelCase.
                    set(&item, "generated_at_ms", doc.generated_at.clone().into());
                    set(&item, "generatedAtMs", doc.generated_at.clone().into());
                    set(
                        &item,
                        "generated_by_user_id",
                        doc.generated_by_user_id.clone().into(),
                    );
                    set(&item, "generatedByUserId", doc.generated_by_user_id.into());
                    set(&item, "content_hash", doc.content_hash.clone().into());
                    set(&item, "contentHash", doc.content_hash.into());
                    set(&item, "revoked_at_ms", doc.revoked_at_ms.clone().into());
                    set(&item, "revokedAtMs", doc.revoked_at_ms.clone().into());
                    arr.push(&item.into());
                }
                set(&obj, "documents", arr.into());
            }
            tentaflow_protocol::LegalAdminPayload::GenerateRequest(_) => {
                set(&obj, "variant", "LegalDocumentGenerateRequest".into());
                set(
                    &obj,
                    "warning",
                    "unexpected_request_variant_in_response".into(),
                );
            }
            tentaflow_protocol::LegalAdminPayload::GenerateResponse(resp) => {
                set(&obj, "variant", "LegalDocumentGenerateResponse".into());
                set(&obj, "doc_id", resp.doc_id.clone().into());
                set(&obj, "docId", resp.doc_id.into());
                set(&obj, "content_hash", resp.content_hash.clone().into());
                set(&obj, "contentHash", resp.content_hash.into());
                set(&obj, "signed_url", resp.signed_url.clone().into());
                set(&obj, "signedUrl", resp.signed_url.into());
            }
            tentaflow_protocol::LegalAdminPayload::RevokeRequest(_) => {
                set(&obj, "variant", "LegalDocumentRevokeRequest".into());
                set(
                    &obj,
                    "warning",
                    "unexpected_request_variant_in_response".into(),
                );
            }
            tentaflow_protocol::LegalAdminPayload::RevokeResponse(resp) => {
                set(&obj, "variant", "LegalDocumentRevokeResponse".into());
                set(&obj, "doc_id", resp.doc_id.clone().into());
                set(&obj, "docId", resp.doc_id.into());
                set(&obj, "revoked_at_ms", resp.revoked_at_ms.clone().into());
                set(&obj, "revokedAtMs", resp.revoked_at_ms.clone().into());
            }
        },
        MessageBody::ComplianceAdminBody(payload) => {
            compliance_admin_payload_to_js(&obj, payload);
        }
        MessageBody::StreamBody(payload) => match payload {
            // Request variants never legitimately decode in a response path.
            // Surface the tag only — same defense-in-depth as
            // CameraAdminPayload request-in-response handling.
            tentaflow_protocol::StreamPayload::SubscribeRequest(_) => {
                set(&obj, "variant", "StreamSubscribeRequest".into());
                set(
                    &obj,
                    "warning",
                    "unexpected_request_variant_in_response".into(),
                );
            }
            tentaflow_protocol::StreamPayload::CloseRequest(_) => {
                set(&obj, "variant", "StreamCloseRequest".into());
                set(
                    &obj,
                    "warning",
                    "unexpected_request_variant_in_response".into(),
                );
            }
            tentaflow_protocol::StreamPayload::SubscribeResponse(resp) => {
                set(&obj, "variant", "StreamSubscribeResponse".into());
                set(&obj, "stream_id", resp.stream_id.clone().into());
                set(&obj, "streamId", resp.stream_id.into());
                set(&obj, "mime_type", resp.mime_type.clone().into());
                set(&obj, "mimeType", resp.mime_type.into());
                set(&obj, "has_init_segment", resp.has_init_segment.into());
                set(&obj, "hasInitSegment", resp.has_init_segment.into());
                // Bazowy PTS osi czasu mediów — frontend odejmuje go od pts_ns
                // detekcji, aby zakotwiczyć overlay na właściwej klatce wideo.
                set_optional_u64(&obj, "base_pts_ns", resp.base_pts_ns);
                set_optional_u64(&obj, "basePtsNs", resp.base_pts_ns);
            }
            tentaflow_protocol::StreamPayload::Frame(frame) => {
                set(&obj, "variant", "StreamFrame".into());
                set(&obj, "stream_id", frame.stream_id.clone().into());
                set(&obj, "streamId", frame.stream_id.into());
                set(&obj, "is_init", frame.is_init.into());
                set(&obj, "isInit", frame.is_init.into());
                set(
                    &obj,
                    "data",
                    js_sys::Uint8Array::from(&frame.data[..]).into(),
                );
            }
            tentaflow_protocol::StreamPayload::Closed(c) => {
                set(&obj, "variant", "StreamClosed".into());
                set(&obj, "stream_id", c.stream_id.clone().into());
                set(&obj, "streamId", c.stream_id.into());
                set(&obj, "reason", c.reason.into());
            }
        },
        MessageBody::RoleCatalogBody(payload) => {
            role_catalog_payload_to_js(&obj, payload);
        }
        MessageBody::BaselineDonorListRequest => {
            set(&obj, "variant", "BaselineDonorListRequest".into());
        }
        MessageBody::BaselineDonorListResponseBody(resp) => {
            set(&obj, "variant", "BaselineDonorListResponse".into());
            let arr = js_sys::Array::new();
            for c in resp.candidates {
                let item = js_sys::Object::new();
                set(&item, "nodeId", c.node_id.clone().into());
                set(&item, "node_id", c.node_id.into());
                set(&item, "displayName", c.display_name.clone().into());
                set(&item, "display_name", c.display_name.into());
                set(&item, "trusted", c.trusted.into());
                if let Some(s) = c.summary {
                    let summary = js_sys::Object::new();
                    set(&summary, "orgName", s.org_name.clone().into());
                    set(&summary, "org_name", s.org_name.into());
                    set(&summary, "users", (s.users as f64).into());
                    set(&summary, "flows", (s.flows as f64).into());
                    set(&summary, "roles", (s.roles as f64).into());
                    set(&item, "summary", summary.into());
                } else {
                    set(&item, "summary", JsValue::NULL);
                }
                arr.push(&item.into());
            }
            set(&obj, "candidates", arr.into());
        }
        MessageBody::BaselineAdoptStartRequestBody(req) => {
            set(&obj, "variant", "BaselineAdoptStartRequest".into());
            set(&obj, "donorNodeId", req.donor_node_id.clone().into());
            set(&obj, "donor_node_id", req.donor_node_id.into());
        }
        MessageBody::BaselineAdoptStartResponseBody(resp) => {
            set(&obj, "variant", "BaselineAdoptStartResponse".into());
            set(&obj, "ok", resp.ok.into());
            set(&obj, "started", resp.started.into());
            set(&obj, "message", resp.message.into());
        }
        MessageBody::BaselineAdoptStatusRequest => {
            set(&obj, "variant", "BaselineAdoptStatusRequest".into());
        }
        MessageBody::BaselineAdoptStatusResponseBody(resp) => {
            set(&obj, "variant", "BaselineAdoptStatusResponse".into());
            set(&obj, "phase", baseline_phase_name(resp.phase).into());
            match resp.peer {
                Some(p) => {
                    set(&obj, "peer", p.clone().into());
                }
                None => set(&obj, "peer", JsValue::NULL),
            }
            match resp.is_joiner {
                Some(j) => {
                    set(&obj, "isJoiner", j.into());
                    set(&obj, "is_joiner", j.into());
                }
                None => {
                    set(&obj, "isJoiner", JsValue::NULL);
                    set(&obj, "is_joiner", JsValue::NULL);
                }
            }
            if let Some(r) = resp.report {
                let report = js_sys::Object::new();
                set(&report, "donorOrgId", r.donor_org_id.clone().into());
                set(&report, "donor_org_id", r.donor_org_id.into());
                set(
                    &report,
                    "usersMergedByEmail",
                    (r.users_merged_by_email as f64).into(),
                );
                set(
                    &report,
                    "users_merged_by_email",
                    (r.users_merged_by_email as f64).into(),
                );
                set(
                    &report,
                    "usersJoinedDonorOrg",
                    (r.users_joined_donor_org as f64).into(),
                );
                set(
                    &report,
                    "users_joined_donor_org",
                    (r.users_joined_donor_org as f64).into(),
                );
                set(
                    &report,
                    "collisionsSuffixed",
                    (r.collisions_suffixed as f64).into(),
                );
                set(
                    &report,
                    "collisions_suffixed",
                    (r.collisions_suffixed as f64).into(),
                );
                set(&obj, "report", report.into());
            } else {
                set(&obj, "report", JsValue::NULL);
            }
        }
        MessageBody::BaselineAdoptClearRequest => {
            set(&obj, "variant", "BaselineAdoptClearRequest".into());
        }
        MessageBody::BaselineAdoptClearResponseBody(resp) => {
            set(&obj, "variant", "BaselineAdoptClearResponse".into());
            set(&obj, "ok", resp.ok.into());
            set(&obj, "cleared", resp.cleared.into());
            set(&obj, "message", resp.message.into());
        }
        MessageBody::AliasConsumerListRequestBody(r) => {
            set(&obj, "variant", "AliasConsumerListRequest".into());
            set(&obj, "aliasId", r.alias_id.into());
            set(&obj, "alias_id", r.alias_id.into());
        }
        MessageBody::AliasConsumerListResponseBody(r) => {
            set(&obj, "variant", "AliasConsumerListResponse".into());
            set(&obj, "aliasId", r.alias_id.into());
            set(&obj, "alias_id", r.alias_id.into());
            let arr = js_sys::Array::new();
            for c in &r.consumers {
                arr.push(&access_consumer_entry_to_js(c));
            }
            set(&obj, "consumers", arr.into());
        }
        MessageBody::AliasConsumerGrantRequestBody(r) => {
            set(&obj, "variant", "AliasConsumerGrantRequest".into());
            set(&obj, "aliasId", r.alias_id.into());
            set(&obj, "alias_id", r.alias_id.into());
            set(&obj, "addonId", r.addon_id.clone().into());
            set(&obj, "addon_id", r.addon_id.into());
        }
        MessageBody::AliasConsumerRevokeRequestBody(r) => {
            set(&obj, "variant", "AliasConsumerRevokeRequest".into());
            set(&obj, "aliasId", r.alias_id.into());
            set(&obj, "alias_id", r.alias_id.into());
            set(&obj, "addonId", r.addon_id.clone().into());
            set(&obj, "addon_id", r.addon_id.into());
        }
        MessageBody::AliasVisibilitySetRequestBody(r) => {
            set(&obj, "variant", "AliasVisibilitySetRequest".into());
            set(&obj, "aliasId", r.alias_id.into());
            set(&obj, "alias_id", r.alias_id.into());
            set(&obj, "visibility", r.visibility.into());
        }
        MessageBody::ModelVisibilityListRequest => {
            set(&obj, "variant", "ModelVisibilityListRequest".into());
        }
        MessageBody::ModelVisibilityListResponseBody(r) => {
            set(&obj, "variant", "ModelVisibilityListResponse".into());
            let arr = js_sys::Array::new();
            for m in &r.models {
                let item = js_sys::Object::new();
                set(&item, "modelId", m.model_id.clone().into());
                set(&item, "model_id", m.model_id.clone().into());
                set(&item, "visibility", m.visibility.clone().into());
                arr.push(&item.into());
            }
            set(&obj, "models", arr.into());
        }
        MessageBody::ModelVisibilitySetRequestBody(r) => {
            set(&obj, "variant", "ModelVisibilitySetRequest".into());
            set(&obj, "modelId", r.model_id.clone().into());
            set(&obj, "model_id", r.model_id.into());
            set(&obj, "visibility", r.visibility.into());
        }
        MessageBody::ModelConsumerListRequestBody(r) => {
            set(&obj, "variant", "ModelConsumerListRequest".into());
            set(&obj, "modelId", r.model_id.clone().into());
            set(&obj, "model_id", r.model_id.into());
        }
        MessageBody::ModelConsumerListResponseBody(r) => {
            set(&obj, "variant", "ModelConsumerListResponse".into());
            set(&obj, "modelId", r.model_id.clone().into());
            set(&obj, "model_id", r.model_id.into());
            let arr = js_sys::Array::new();
            for c in &r.consumers {
                arr.push(&access_consumer_entry_to_js(c));
            }
            set(&obj, "consumers", arr.into());
        }
        MessageBody::ModelConsumerGrantRequestBody(r) => {
            set(&obj, "variant", "ModelConsumerGrantRequest".into());
            set(&obj, "modelId", r.model_id.clone().into());
            set(&obj, "model_id", r.model_id.into());
            set(&obj, "addonId", r.addon_id.clone().into());
            set(&obj, "addon_id", r.addon_id.into());
        }
        MessageBody::ModelConsumerRevokeRequestBody(r) => {
            set(&obj, "variant", "ModelConsumerRevokeRequest".into());
            set(&obj, "modelId", r.model_id.clone().into());
            set(&obj, "model_id", r.model_id.into());
            set(&obj, "addonId", r.addon_id.clone().into());
            set(&obj, "addon_id", r.addon_id.into());
        }
        MessageBody::AddonAccessListRequestBody(r) => {
            set(&obj, "variant", "AddonAccessListRequest".into());
            set(&obj, "addonId", r.addon_id.clone().into());
            set(&obj, "addon_id", r.addon_id.into());
        }
        MessageBody::AddonAccessListResponseBody(r) => {
            set(&obj, "variant", "AddonAccessListResponse".into());
            set(&obj, "addonId", r.addon_id.clone().into());
            set(&obj, "addon_id", r.addon_id.into());
            let alias_arr = js_sys::Array::new();
            for u in &r.uses_alias {
                alias_arr.push(&addon_uses_entry_to_js(u));
            }
            set(&obj, "usesAlias", alias_arr.clone().into());
            set(&obj, "uses_alias", alias_arr.into());
            let model_arr = js_sys::Array::new();
            for u in &r.uses_model {
                model_arr.push(&addon_uses_entry_to_js(u));
            }
            set(&obj, "usesModel", model_arr.clone().into());
            set(&obj, "uses_model", model_arr.into());
        }
        MessageBody::AddonAccessDecisionRequestBody(r) => {
            set(&obj, "variant", "AddonAccessDecisionRequest".into());
            set(&obj, "addonId", r.addon_id.clone().into());
            set(&obj, "addon_id", r.addon_id.into());
            set(&obj, "kind", r.kind.into());
            set(&obj, "target", r.target.into());
            set(&obj, "decision", r.decision.into());
        }
        MessageBody::AccessMutationResponseBody(r) => {
            set(&obj, "variant", "AccessMutationResponse".into());
            set(&obj, "ok", r.ok.into());
            let arr = js_sys::Array::new();
            for t in &r.transitions {
                arr.push(&access_transition_to_js(t));
            }
            set(&obj, "transitions", arr.into());
        }
        MessageBody::UiChannelCbor(bytes) => {
            set(&obj, "variant", "UiChannelCbor".into());
            set(&obj, "cbor", js_sys::Uint8Array::from(&bytes[..]).into());
        }
        MessageBody::MlStudioBody(payload) => decode_ml_studio_payload(&obj, payload),
        MessageBody::AddonDocumentBody(AddonDocumentPayload::UploadChunkRequest(req)) => {
            set(&obj, "variant", "AddonDocumentUploadChunkRequest".into());
            set(&obj, "addonId", req.addon_id.clone().into());
            set(&obj, "addon_id", req.addon_id.into());
            set(&obj, "uploadId", req.upload_id.clone().into());
            set(&obj, "upload_id", req.upload_id.into());
            set(&obj, "filename", req.filename.into());
            set(&obj, "mime", req.mime.into());
            set(&obj, "seq", req.seq.into());
            set(&obj, "totalChunks", req.total_chunks.into());
            set(&obj, "total_chunks", req.total_chunks.into());
        }
        MessageBody::AddonDocumentBody(AddonDocumentPayload::UploadChunkResponse(resp)) => {
            set(&obj, "variant", "AddonDocumentUploadChunkResponse".into());
            set(&obj, "uploadId", resp.upload_id.clone().into());
            set(&obj, "upload_id", resp.upload_id.into());
            set(&obj, "receivedChunks", resp.received_chunks.into());
            set(&obj, "received_chunks", resp.received_chunks.into());
            set(&obj, "receivedBytes", (resp.received_bytes as f64).into());
            set(&obj, "received_bytes", (resp.received_bytes as f64).into());
            match resp.doc_ref {
                Some(r) => {
                    set(&obj, "docRef", r.clone().into());
                    set(&obj, "doc_ref", r.into());
                }
                None => {
                    set(&obj, "docRef", JsValue::NULL);
                    set(&obj, "doc_ref", JsValue::NULL);
                }
            }
        }
        MessageBody::RobotsBody(payload) => decode_robots_payload(&obj, payload),
    }
    Ok(obj.into())
}

fn robot_entry_to_js(r: &tentaflow_protocol::RobotEntry) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "robotId", r.robot_id.clone().into());
    set(&obj, "robot_id", r.robot_id.clone().into());
    set(&obj, "ownerNodeId", r.owner_node_id.clone().into());
    set(&obj, "owner_node_id", r.owner_node_id.clone().into());
    set(&obj, "isLocal", r.is_local.into());
    set(&obj, "is_local", r.is_local.into());
    match &r.kind {
        Some(k) => set(&obj, "kind", k.clone().into()),
        None => set(&obj, "kind", JsValue::NULL),
    }
    set(&obj, "status", r.status.clone().into());
    match r.battery_percent {
        Some(b) => {
            set(&obj, "batteryPercent", (b as f64).into());
            set(&obj, "battery_percent", (b as f64).into());
        }
        None => {
            set(&obj, "batteryPercent", JsValue::NULL);
            set(&obj, "battery_percent", JsValue::NULL);
        }
    }
    match r.rtt_ms {
        Some(rtt) => {
            set(&obj, "rttMs", (rtt as f64).into());
            set(&obj, "rtt_ms", (rtt as f64).into());
        }
        None => {
            set(&obj, "rttMs", JsValue::NULL);
            set(&obj, "rtt_ms", JsValue::NULL);
        }
    }
    match &r.camera_id {
        Some(c) => {
            set(&obj, "cameraId", c.clone().into());
            set(&obj, "camera_id", c.clone().into());
        }
        None => {
            set(&obj, "cameraId", JsValue::NULL);
            set(&obj, "camera_id", JsValue::NULL);
        }
    }
    let caps = js_sys::Array::new();
    for c in &r.capabilities {
        caps.push(&JsValue::from(c.clone()));
    }
    set(&obj, "capabilities", caps.into());
    let actions = js_sys::Array::new();
    for a in &r.actions_meta {
        actions.push(&robot_action_meta_to_js(a));
    }
    set(&obj, "actionsMeta", actions.clone().into());
    set(&obj, "actions_meta", actions.into());
    match &r.lidar {
        Some(l) => set(&obj, "lidar", robot_lidar_to_js(l)),
        None => set(&obj, "lidar", JsValue::NULL),
    }
    match &r.telemetry {
        Some(t) => set(&obj, "telemetry", robot_telemetry_to_js(t)),
        None => set(&obj, "telemetry", JsValue::NULL),
    }
    obj
}

/// Structured telemetry snapshot → JS object (camel + snake keys). Mirrors
/// `robot_lidar_to_js`: present scalars are emitted, absent ones become NULL, and
/// the nested velocity/imu/battery objects are only created when their source
/// fields are present (capability-absent → omitted, never fabricated).
fn robot_telemetry_to_js(t: &tentaflow_protocol::RobotTelemetrySnapshot) -> JsValue {
    let obj = js_sys::Object::new();
    match t.mode {
        Some(m) => set(&obj, "mode", (m as f64).into()),
        None => set(&obj, "mode", JsValue::NULL),
    }
    match t.gait_type {
        Some(g) => {
            set(&obj, "gaitType", (g as f64).into());
            set(&obj, "gait_type", (g as f64).into());
        }
        None => {
            set(&obj, "gaitType", JsValue::NULL);
            set(&obj, "gait_type", JsValue::NULL);
        }
    }
    match t.body_height {
        Some(h) => {
            set(&obj, "bodyHeight", h.into());
            set(&obj, "body_height", h.into());
        }
        None => {
            set(&obj, "bodyHeight", JsValue::NULL);
            set(&obj, "body_height", JsValue::NULL);
        }
    }
    let position = js_sys::Array::new();
    for v in &t.position {
        position.push(&JsValue::from(*v));
    }
    set(&obj, "position", position.into());
    let foot_force = js_sys::Array::new();
    for v in &t.foot_force {
        foot_force.push(&JsValue::from(*v));
    }
    set(&obj, "footForce", foot_force.clone().into());
    set(&obj, "foot_force", foot_force.into());
    let joints = js_sys::Array::new();
    for v in &t.joints {
        joints.push(&JsValue::from(*v));
    }
    set(&obj, "joints", joints.into());
    let pose_position = js_sys::Array::new();
    for v in &t.pose_position {
        pose_position.push(&JsValue::from(*v));
    }
    set(&obj, "posePosition", pose_position.clone().into());
    set(&obj, "pose_position", pose_position.into());
    let pose_orientation = js_sys::Array::new();
    for v in &t.pose_orientation {
        pose_orientation.push(&JsValue::from(*v));
    }
    set(&obj, "poseOrientation", pose_orientation.clone().into());
    set(&obj, "pose_orientation", pose_orientation.into());
    match t.vx {
        Some(vx) => set(&obj, "vx", vx.into()),
        None => set(&obj, "vx", JsValue::NULL),
    }
    match t.vy {
        Some(vy) => set(&obj, "vy", vy.into()),
        None => set(&obj, "vy", JsValue::NULL),
    }
    match t.vyaw {
        Some(vyaw) => set(&obj, "vyaw", vyaw.into()),
        None => set(&obj, "vyaw", JsValue::NULL),
    }
    if t.vx.is_some() || t.vy.is_some() || t.vyaw.is_some() {
        let vel = js_sys::Object::new();
        if let Some(vx) = t.vx {
            set(&vel, "vx", vx.into());
        }
        if let Some(vy) = t.vy {
            set(&vel, "vy", vy.into());
        }
        if let Some(vyaw) = t.vyaw {
            set(&vel, "vyaw", vyaw.into());
        }
        set(&obj, "velocity", vel.into());
    }
    if let Some(imu) = &t.imu {
        let io = js_sys::Object::new();
        if let Some(roll) = imu.roll {
            set(&io, "roll", roll.into());
        }
        if let Some(pitch) = imu.pitch {
            set(&io, "pitch", pitch.into());
        }
        if let Some(yaw) = imu.yaw {
            set(&io, "yaw", yaw.into());
        }
        if let Some(temp) = imu.temperature {
            set(&io, "temperature", temp.into());
        }
        let quat = js_sys::Array::new();
        for v in &imu.quaternion {
            quat.push(&JsValue::from(*v));
        }
        set(&io, "quaternion", quat.into());
        set(&obj, "imu", io.into());
    }
    if let Some(bat) = &t.battery {
        let bo = js_sys::Object::new();
        if let Some(soc) = bat.soc {
            set(&bo, "soc", soc.into());
        }
        if let Some(voltage) = bat.voltage {
            set(&bo, "voltage", voltage.into());
        }
        if let Some(current) = bat.current {
            set(&bo, "current", current.into());
        }
        if let Some(temp) = bat.temperature {
            set(&bo, "temperature", temp.into());
        }
        set(&obj, "battery", bo.into());
    }
    obj.into()
}

/// SMALL LiDAR availability snapshot → JS object (camel + snake keys). Never the
/// point cloud — only the metadata the card needs and a renderer pulls on demand.
fn robot_lidar_to_js(l: &tentaflow_protocol::RobotLidarStatus) -> JsValue {
    let obj = js_sys::Object::new();
    set(&obj, "enabled", l.enabled.into());
    set(&obj, "available", l.available.into());
    set(&obj, "pointCount", (l.point_count as f64).into());
    set(&obj, "point_count", (l.point_count as f64).into());
    match l.resolution {
        Some(r) => set(&obj, "resolution", (r as f64).into()),
        None => set(&obj, "resolution", JsValue::NULL),
    }
    let origin = js_sys::Array::new();
    for v in &l.origin {
        origin.push(&JsValue::from(*v));
    }
    set(&obj, "origin", origin.into());
    set(&obj, "frameSeq", (l.frame_seq as f64).into());
    set(&obj, "frame_seq", (l.frame_seq as f64).into());
    set(&obj, "lastUpdateTs", (l.last_update_ts as f64).into());
    set(&obj, "last_update_ts", (l.last_update_ts as f64).into());
    obj.into()
}

fn robot_action_meta_to_js(a: &tentaflow_protocol::RobotActionMeta) -> JsValue {
    let obj = js_sys::Object::new();
    set(&obj, "kind", a.kind.clone().into());
    set(&obj, "label", a.label.clone().into());
    set(&obj, "risk", a.risk.clone().into());
    set(&obj, "acrobatic", a.acrobatic.into());
    set(&obj, "readOnly", a.read_only.into());
    set(&obj, "read_only", a.read_only.into());
    let params = js_sys::Array::new();
    for p in &a.params {
        let pobj = js_sys::Object::new();
        set(&pobj, "name", p.name.clone().into());
        set(&pobj, "min", p.min.into());
        set(&pobj, "max", p.max.into());
        params.push(&pobj.into());
    }
    set(&obj, "params", params.into());
    obj.into()
}

fn decode_robots_payload(obj: &js_sys::Object, payload: tentaflow_protocol::RobotsPayload) {
    use tentaflow_protocol::RobotsPayload as P;
    match payload {
        P::ListRequest(_) => set(obj, "variant", "RobotsListRequest".into()),
        P::ListResponse(resp) => {
            set(obj, "variant", "RobotsListResponse".into());
            let arr = js_sys::Array::new();
            for r in &resp.robots {
                arr.push(&robot_entry_to_js(r));
            }
            set(obj, "robots", arr.into());
        }
        P::ControlRequest(req) => {
            set(obj, "variant", "RobotControlRequest".into());
            set(obj, "robotId", req.robot_id.clone().into());
            set(obj, "robot_id", req.robot_id.into());
            set(obj, "kind", req.action.kind.into());
        }
        P::ControlResponse(resp) => {
            set(obj, "variant", "RobotControlResponse".into());
            set(obj, "ok", resp.ok.into());
            match resp.rejected {
                Some(r) => set(obj, "rejected", r.into()),
                None => set(obj, "rejected", JsValue::NULL),
            }
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
            // Read-only actions (lidar_frame) return their JSON payload here.
            match resp.result {
                Some(r) => set(obj, "result", r.into()),
                None => set(obj, "result", JsValue::NULL),
            }
        }
        P::CameraShareRequest(req) => {
            set(obj, "variant", "RobotCameraShareRequest".into());
            set(obj, "robotId", req.robot_id.clone().into());
            set(obj, "robot_id", req.robot_id.into());
            set(obj, "cameraId", req.camera_id.clone().into());
            set(obj, "camera_id", req.camera_id.into());
        }
        P::CameraShareResponse(resp) => {
            set(obj, "variant", "RobotCameraShareResponse".into());
            set(obj, "ok", resp.ok.into());
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
            match resp.note {
                Some(n) => set(obj, "note", n.into()),
                None => set(obj, "note", JsValue::NULL),
            }
        }
        P::GeoAnchorSetRequest(req) => {
            set(obj, "variant", "RobotGeoAnchorSetRequest".into());
            set(obj, "robotId", req.robot_id.clone().into());
            set(obj, "robot_id", req.robot_id.into());
        }
        P::GeoAnchorGetRequest(req) => {
            set(obj, "variant", "RobotGeoAnchorGetRequest".into());
            set(obj, "robotId", req.robot_id.clone().into());
            set(obj, "robot_id", req.robot_id.into());
        }
        P::GeoAnchorResponse(resp) => {
            set(obj, "variant", "RobotGeoAnchorResponse".into());
            set(obj, "ok", resp.ok.into());
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
            set(obj, "anchored", resp.anchored.into());
            let num = |o: &js_sys::Object, k: &str, v: Option<f64>| match v {
                Some(x) => set(o, k, x.into()),
                None => set(o, k, JsValue::NULL),
            };
            num(obj, "lat", resp.lat);
            num(obj, "lon", resp.lon);
            num(obj, "alt", resp.alt);
            num(obj, "heading", resp.heading);
            num(obj, "poseLat", resp.pose_lat);
            num(obj, "poseLon", resp.pose_lon);
            num(obj, "poseAlt", resp.pose_alt);
        }
    }
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
    set(&obj, "frame_seq", header.frame_seq.into());
    set(&obj, "pointCount", header.point_count.into());
    set(&obj, "point_count", header.point_count.into());
    set(&obj, "layout", header.layout.into());
    set(&obj, "resolution", header.resolution.into());
    let origin = js_sys::Array::new();
    for v in &header.origin {
        origin.push(&JsValue::from(*v as f64));
    }
    set(&obj, "origin", origin.into());
    set(&obj, "timestampUs", (header.timestamp_us as f64).into());
    set(&obj, "timestamp_us", (header.timestamp_us as f64).into());
    set(&obj, "hostSendUs", (header.host_send_us as f64).into());
    set(&obj, "host_send_us", (header.host_send_us as f64).into());
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
    set(&obj, "points", js_sys::Float32Array::from(&floats[..]).into());
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

fn ml_studio_summary_to_js(s: &tentaflow_protocol::MlStudioProjectSummary) -> js_sys::Object {
    let item = js_sys::Object::new();
    set(&item, "projectId", s.project_id.clone().into());
    set(&item, "project_id", s.project_id.clone().into());
    set(&item, "name", s.name.clone().into());
    set(&item, "description", s.description.clone().into());
    set(&item, "projectType", s.project_type.clone().into());
    set(&item, "project_type", s.project_type.clone().into());
    set(&item, "status", s.status.clone().into());
    set(&item, "datasetCount", s.dataset_count.into());
    set(&item, "dataset_count", s.dataset_count.into());
    set(&item, "modelCount", s.model_count.into());
    set(&item, "model_count", s.model_count.into());
    set(&item, "trainingCount", s.training_count.into());
    set(&item, "training_count", s.training_count.into());
    set(&item, "createdAt", s.created_at.clone().into());
    set(&item, "created_at", s.created_at.clone().into());
    set(&item, "updatedAt", s.updated_at.clone().into());
    set(&item, "updated_at", s.updated_at.clone().into());
    set(&item, "role", s.role.clone().into());
    set(&item, "isOwner", s.is_owner.into());
    set(&item, "is_owner", s.is_owner.into());
    item
}

fn ml_studio_detail_to_js(d: &tentaflow_protocol::MlStudioProjectDetail) -> js_sys::Object {
    let item = js_sys::Object::new();
    set(&item, "projectId", d.project_id.clone().into());
    set(&item, "project_id", d.project_id.clone().into());
    set(&item, "name", d.name.clone().into());
    set(&item, "description", d.description.clone().into());
    set(&item, "projectType", d.project_type.clone().into());
    set(&item, "project_type", d.project_type.clone().into());
    set(&item, "status", d.status.clone().into());
    set(&item, "ownerUserId", d.owner_user_id.clone().into());
    set(&item, "owner_user_id", d.owner_user_id.clone().into());
    set(&item, "orgId", d.org_id.clone().into());
    set(&item, "org_id", d.org_id.clone().into());
    set(&item, "datasetCount", d.dataset_count.into());
    set(&item, "dataset_count", d.dataset_count.into());
    set(&item, "modelCount", d.model_count.into());
    set(&item, "model_count", d.model_count.into());
    set(&item, "trainingCount", d.training_count.into());
    set(&item, "training_count", d.training_count.into());
    set(&item, "createdAt", d.created_at.clone().into());
    set(&item, "created_at", d.created_at.clone().into());
    set(&item, "updatedAt", d.updated_at.clone().into());
    set(&item, "updated_at", d.updated_at.clone().into());
    set(&item, "role", d.role.clone().into());
    set(&item, "isOwner", d.is_owner.into());
    set(&item, "is_owner", d.is_owner.into());
    item
}

fn ml_studio_member_to_js(m: &tentaflow_protocol::MlStudioProjectMember) -> js_sys::Object {
    let item = js_sys::Object::new();
    set(&item, "userId", m.user_id.clone().into());
    set(&item, "user_id", m.user_id.clone().into());
    set(&item, "displayName", m.display_name.clone().into());
    set(&item, "display_name", m.display_name.clone().into());
    set(&item, "role", m.role.clone().into());
    set(&item, "status", m.status.clone().into());
    set(&item, "invitedBy", m.invited_by.clone().into());
    set(&item, "invited_by", m.invited_by.clone().into());
    set(&item, "createdAt", m.created_at.clone().into());
    set(&item, "created_at", m.created_at.clone().into());
    item
}

fn ml_studio_grant_to_js(g: &tentaflow_protocol::MlStudioResourceGrant) -> js_sys::Object {
    let item = js_sys::Object::new();
    set(&item, "grantId", g.grant_id.clone().into());
    set(&item, "grant_id", g.grant_id.clone().into());
    set(&item, "subjectKind", g.subject_kind.clone().into());
    set(&item, "subject_kind", g.subject_kind.clone().into());
    set(&item, "subjectId", g.subject_id.clone().into());
    set(&item, "subject_id", g.subject_id.clone().into());
    set(&item, "nodeId", g.node_id.clone().into());
    set(&item, "node_id", g.node_id.clone().into());
    set(&item, "resourceKind", g.resource_kind.clone().into());
    set(&item, "resource_kind", g.resource_kind.clone().into());
    set(&item, "resourceRef", g.resource_ref.clone().into());
    set(&item, "resource_ref", g.resource_ref.clone().into());
    set(&item, "quota", g.quota.clone().into());
    set(&item, "grantedBy", g.granted_by.clone().into());
    set(&item, "granted_by", g.granted_by.clone().into());
    set(&item, "createdAt", g.created_at.clone().into());
    set(&item, "created_at", g.created_at.clone().into());
    item
}

/// Mapuje statystyki GPU na obiekt JS (camelCase + snake_case).
fn ml_studio_gpu_stats_to_js(g: &tentaflow_protocol::GpuStats) -> js_sys::Object {
    let item = js_sys::Object::new();
    set(&item, "name", g.name.clone().into());
    set(&item, "memUsedMb", (g.mem_used_mb as f64).into());
    set(&item, "mem_used_mb", (g.mem_used_mb as f64).into());
    set(&item, "memTotalMb", (g.mem_total_mb as f64).into());
    set(&item, "mem_total_mb", (g.mem_total_mb as f64).into());
    set(&item, "utilPct", (g.util_pct as f64).into());
    set(&item, "util_pct", (g.util_pct as f64).into());
    item
}

/// Mapuje jeden aktywny job treningowy (panel jobów) na obiekt JS.
fn ml_studio_job_info_to_js(j: &tentaflow_protocol::TrainingJobInfo) -> js_sys::Object {
    let item = js_sys::Object::new();
    set(&item, "runId", j.run_id.clone().into());
    set(&item, "run_id", j.run_id.clone().into());
    set(&item, "projectId", j.project_id.clone().into());
    set(&item, "project_id", j.project_id.clone().into());
    set(&item, "projectName", j.project_name.clone().into());
    set(&item, "project_name", j.project_name.clone().into());
    set(&item, "kind", j.kind.clone().into());
    set(&item, "variant", j.variant.clone().into());
    set(&item, "status", j.status.clone().into());
    set(&item, "epoch", (j.epoch as f64).into());
    set(&item, "totalEpochs", (j.total_epochs as f64).into());
    set(&item, "total_epochs", (j.total_epochs as f64).into());
    set(&item, "etaS", (j.eta_s as f64).into());
    set(&item, "eta_s", (j.eta_s as f64).into());
    set(&item, "elapsedS", (j.elapsed_s as f64).into());
    set(&item, "elapsed_s", (j.elapsed_s as f64).into());
    set(&item, "gpuMemMb", (j.gpu_mem_mb as f64).into());
    set(&item, "gpu_mem_mb", (j.gpu_mem_mb as f64).into());
    set(&item, "stage", j.stage.clone().into());
    set(&item, "startedAt", j.started_at.clone().into());
    set(&item, "started_at", j.started_at.clone().into());
    item
}

fn ml_studio_training_run_to_js(
    r: &tentaflow_protocol::MlStudioTrainingRunSummary,
) -> js_sys::Object {
    let item = js_sys::Object::new();
    set(&item, "runId", r.run_id.clone().into());
    set(&item, "run_id", r.run_id.clone().into());
    let model_id: JsValue = r
        .model_id
        .clone()
        .map(JsValue::from)
        .unwrap_or(JsValue::NULL);
    set(&item, "modelId", model_id.clone());
    set(&item, "model_id", model_id);
    set(&item, "status", r.status.clone().into());
    set(&item, "configJson", r.config_json.clone().into());
    set(&item, "config_json", r.config_json.clone().into());
    let started_at: JsValue = r
        .started_at
        .clone()
        .map(JsValue::from)
        .unwrap_or(JsValue::NULL);
    set(&item, "startedAt", started_at.clone());
    set(&item, "started_at", started_at);
    let finished_at: JsValue = r
        .finished_at
        .clone()
        .map(JsValue::from)
        .unwrap_or(JsValue::NULL);
    set(&item, "finishedAt", finished_at.clone());
    set(&item, "finished_at", finished_at);
    item
}

fn ml_studio_model_to_js(m: &tentaflow_protocol::MlStudioModelSummary) -> js_sys::Object {
    let item = js_sys::Object::new();
    set(&item, "modelId", m.model_id.clone().into());
    set(&item, "model_id", m.model_id.clone().into());
    set(&item, "name", m.name.clone().into());
    set(&item, "framework", m.framework.clone().into());
    set(&item, "baseModel", m.base_model.clone().into());
    set(&item, "base_model", m.base_model.clone().into());
    set(&item, "status", m.status.clone().into());
    set(&item, "metricsJson", m.metrics_json.clone().into());
    set(&item, "metrics_json", m.metrics_json.clone().into());
    set(&item, "createdAt", m.created_at.clone().into());
    set(&item, "created_at", m.created_at.clone().into());
    item
}

fn ml_studio_dataset_summary_to_js(d: &tentaflow_protocol::DatasetSummary) -> js_sys::Object {
    let item = js_sys::Object::new();
    set(&item, "datasetId", d.dataset_id.clone().into());
    set(&item, "dataset_id", d.dataset_id.clone().into());
    set(&item, "projectId", d.project_id.clone().into());
    set(&item, "project_id", d.project_id.clone().into());
    set(&item, "name", d.name.clone().into());
    set(&item, "kind", d.kind.clone().into());
    set(&item, "rowCount", (d.row_count as f64).into());
    set(&item, "row_count", (d.row_count as f64).into());
    set(&item, "columnCount", d.column_count.into());
    set(&item, "column_count", d.column_count.into());
    set(&item, "createdAt", d.created_at.clone().into());
    set(&item, "created_at", d.created_at.clone().into());
    set(&item, "profileJson", d.profile_json.clone().into());
    set(&item, "profile_json", d.profile_json.clone().into());
    item
}

fn ml_studio_table_profile_to_js(p: &tentaflow_protocol::TableProfile) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "format", p.format.clone().into());
    set(&obj, "rowCount", (p.row_count as f64).into());
    set(&obj, "row_count", (p.row_count as f64).into());
    set(&obj, "scannedRows", (p.scanned_rows as f64).into());
    set(&obj, "scanned_rows", (p.scanned_rows as f64).into());
    set(&obj, "columnCount", p.column_count.into());
    set(&obj, "column_count", p.column_count.into());
    set(&obj, "truncated", p.truncated.into());
    let columns = js_sys::Array::new();
    for c in &p.columns {
        let col = js_sys::Object::new();
        set(&col, "name", c.name.clone().into());
        set(&col, "columnType", c.column_type.clone().into());
        set(&col, "column_type", c.column_type.clone().into());
        set(&col, "uniqueCount", (c.unique_count as f64).into());
        set(&col, "unique_count", (c.unique_count as f64).into());
        set(&col, "missingRatio", c.missing_ratio.into());
        set(&col, "missing_ratio", c.missing_ratio.into());
        set(&col, "uniqueCapped", c.unique_capped.into());
        set(&col, "unique_capped", c.unique_capped.into());
        let examples = js_sys::Array::new();
        for ex in &c.examples {
            examples.push(&ex.clone().into());
        }
        set(&col, "examples", examples.into());
        let classes = js_sys::Array::new();
        for cc in &c.classes {
            let entry = js_sys::Object::new();
            set(&entry, "value", cc.value.clone().into());
            set(&entry, "count", (cc.count as f64).into());
            classes.push(&entry);
        }
        set(&col, "classes", classes.into());
        columns.push(&col);
    }
    set(&obj, "columns", columns.into());
    obj
}

fn decode_ml_studio_payload(obj: &js_sys::Object, payload: tentaflow_protocol::MlStudioPayload) {
    match payload {
        tentaflow_protocol::MlStudioPayload::ProjectsListRequest(_) => {
            set(obj, "variant", "MlStudioProjectsListRequest".into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectsListResponse(resp) => {
            set(obj, "variant", "MlStudioProjectsListResponse".into());
            let arr = js_sys::Array::new();
            for s in &resp.projects {
                arr.push(&ml_studio_summary_to_js(s));
            }
            set(obj, "projects", arr.into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectCreateRequest(req) => {
            set(obj, "variant", "MlStudioProjectCreateRequest".into());
            set(obj, "name", req.name.into());
            set(obj, "description", req.description.into());
            set(obj, "projectType", req.project_type.clone().into());
            set(obj, "project_type", req.project_type.into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectCreateResponse(resp) => {
            set(obj, "variant", "MlStudioProjectCreateResponse".into());
            set(obj, "project", ml_studio_detail_to_js(&resp.project).into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectDetailRequest(req) => {
            set(obj, "variant", "MlStudioProjectDetailRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectDetailResponse(resp) => {
            set(obj, "variant", "MlStudioProjectDetailResponse".into());
            set(obj, "project", ml_studio_detail_to_js(&resp.project).into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectTypesListRequest(_) => {
            set(obj, "variant", "MlStudioProjectTypesListRequest".into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectTypesListResponse(resp) => {
            set(obj, "variant", "MlStudioProjectTypesListResponse".into());
            let arr = js_sys::Array::new();
            for t in &resp.types {
                let item = js_sys::Object::new();
                set(&item, "slug", t.slug.clone().into());
                set(&item, "label", t.label.clone().into());
                set(&item, "description", t.description.clone().into());
                arr.push(&item);
            }
            set(obj, "types", arr.into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectMembersListRequest(req) => {
            set(obj, "variant", "MlStudioProjectMembersListRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectMembersListResponse(resp) => {
            set(obj, "variant", "MlStudioProjectMembersListResponse".into());
            let arr = js_sys::Array::new();
            for m in &resp.members {
                arr.push(&ml_studio_member_to_js(m));
            }
            set(obj, "members", arr.into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectInviteRequest(req) => {
            set(obj, "variant", "MlStudioProjectInviteRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "inviteeUserId", req.invitee_user_id.clone().into());
            set(obj, "invitee_user_id", req.invitee_user_id.into());
            set(obj, "role", req.role.into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectInviteResponse(resp) => {
            set(obj, "variant", "MlStudioProjectInviteResponse".into());
            set(obj, "member", ml_studio_member_to_js(&resp.member).into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectMemberRemoveRequest(req) => {
            set(obj, "variant", "MlStudioProjectMemberRemoveRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "userId", req.user_id.clone().into());
            set(obj, "user_id", req.user_id.into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectMemberRemoveResponse(resp) => {
            set(obj, "variant", "MlStudioProjectMemberRemoveResponse".into());
            set(obj, "projectId", resp.project_id.clone().into());
            set(obj, "project_id", resp.project_id.into());
            set(obj, "userId", resp.user_id.clone().into());
            set(obj, "user_id", resp.user_id.into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectMemberRoleSetRequest(req) => {
            set(obj, "variant", "MlStudioProjectMemberRoleSetRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "userId", req.user_id.clone().into());
            set(obj, "user_id", req.user_id.into());
            set(obj, "role", req.role.into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectMemberRoleSetResponse(resp) => {
            set(obj, "variant", "MlStudioProjectMemberRoleSetResponse".into());
            set(obj, "member", ml_studio_member_to_js(&resp.member).into());
        }
        tentaflow_protocol::MlStudioPayload::DatasetUploadRequest(req) => {
            set(obj, "variant", "MlStudioDatasetUploadRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "name", req.name.into());
            set(obj, "filename", req.filename.into());
        }
        tentaflow_protocol::MlStudioPayload::DatasetUploadResponse(resp) => {
            set(obj, "variant", "MlStudioDatasetUploadResponse".into());
            set(obj, "dataset", ml_studio_dataset_summary_to_js(&resp.dataset).into());
        }
        tentaflow_protocol::MlStudioPayload::DatasetUploadChunkRequest(req) => {
            set(obj, "variant", "MlStudioDatasetUploadChunkRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "name", req.name.into());
            set(obj, "filename", req.filename.into());
            set(obj, "uploadId", req.upload_id.clone().into());
            set(obj, "upload_id", req.upload_id.into());
            set(obj, "seq", req.seq.into());
            set(obj, "totalChunks", req.total_chunks.into());
            set(obj, "total_chunks", req.total_chunks.into());
        }
        tentaflow_protocol::MlStudioPayload::DatasetUploadChunkResponse(resp) => {
            set(obj, "variant", "MlStudioDatasetUploadChunkResponse".into());
            set(obj, "uploadId", resp.upload_id.clone().into());
            set(obj, "upload_id", resp.upload_id.into());
            set(obj, "receivedChunks", resp.received_chunks.into());
            set(obj, "received_chunks", resp.received_chunks.into());
            set(obj, "receivedBytes", (resp.received_bytes as f64).into());
            set(obj, "received_bytes", (resp.received_bytes as f64).into());
            if let Some(ds) = &resp.dataset {
                set(obj, "dataset", ml_studio_dataset_summary_to_js(ds).into());
            }
        }
        tentaflow_protocol::MlStudioPayload::DatasetsListRequest(req) => {
            set(obj, "variant", "MlStudioDatasetsListRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
        }
        tentaflow_protocol::MlStudioPayload::DatasetsListResponse(resp) => {
            set(obj, "variant", "MlStudioDatasetsListResponse".into());
            let arr = js_sys::Array::new();
            for d in &resp.datasets {
                arr.push(&ml_studio_dataset_summary_to_js(d));
            }
            set(obj, "datasets", arr.into());
        }
        tentaflow_protocol::MlStudioPayload::DatasetProfileRequest(req) => {
            set(obj, "variant", "MlStudioDatasetProfileRequest".into());
            set(obj, "datasetId", req.dataset_id.clone().into());
            set(obj, "dataset_id", req.dataset_id.into());
        }
        tentaflow_protocol::MlStudioPayload::DatasetProfileResponse(resp) => {
            set(obj, "variant", "MlStudioDatasetProfileResponse".into());
            set(obj, "dataset", ml_studio_dataset_summary_to_js(&resp.dataset).into());
            set(obj, "profile", ml_studio_table_profile_to_js(&resp.profile).into());
        }
        tentaflow_protocol::MlStudioPayload::TabularTrainRequest(req) => {
            set(obj, "variant", "MlStudioTabularTrainRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "datasetId", req.dataset_id.clone().into());
            set(obj, "dataset_id", req.dataset_id.into());
            set(obj, "targetColumn", req.target_column.clone().into());
            set(obj, "target_column", req.target_column.into());
            set(obj, "task", req.task.into());
            if let Some(engine) = req.engine {
                set(obj, "engine", engine.into());
            }
        }
        tentaflow_protocol::MlStudioPayload::TabularTrainResponse(resp) => {
            set(obj, "variant", "MlStudioTabularTrainResponse".into());
            set(obj, "runId", resp.run_id.clone().into());
            set(obj, "run_id", resp.run_id.into());
            set(obj, "bestModelId", resp.best_model_id.clone().into());
            set(obj, "best_model_id", resp.best_model_id.into());
            set(obj, "bestModelName", resp.best_model_name.clone().into());
            set(obj, "best_model_name", resp.best_model_name.into());
            set(obj, "task", resp.task.into());
            set(obj, "targetColumn", resp.target_column.clone().into());
            set(obj, "target_column", resp.target_column.into());
            set(obj, "trainRows", (resp.train_rows as f64).into());
            set(obj, "train_rows", (resp.train_rows as f64).into());
            set(obj, "holdoutRows", (resp.holdout_rows as f64).into());
            set(obj, "holdout_rows", (resp.holdout_rows as f64).into());
            let arr = js_sys::Array::new();
            for e in &resp.leaderboard {
                let row = js_sys::Object::new();
                set(&row, "modelName", e.model_name.clone().into());
                set(&row, "model_name", e.model_name.clone().into());
                set(&row, "framework", e.framework.clone().into());
                set(&row, "accuracy", opt_f64_to_js(e.accuracy));
                set(&row, "f1Macro", opt_f64_to_js(e.f1_macro));
                set(&row, "f1_macro", opt_f64_to_js(e.f1_macro));
                set(&row, "rmse", opt_f64_to_js(e.rmse));
                set(&row, "r2", opt_f64_to_js(e.r2));
                set(&row, "trainSecs", e.train_secs.into());
                set(&row, "train_secs", e.train_secs.into());
                arr.push(&row);
            }
            set(obj, "leaderboard", arr.into());
        }
        tentaflow_protocol::MlStudioPayload::ResourceGrantCreateRequest(req) => {
            set(obj, "variant", "MlStudioResourceGrantCreateRequest".into());
            set(obj, "subjectKind", req.subject_kind.clone().into());
            set(obj, "subject_kind", req.subject_kind.clone().into());
            set(obj, "subjectId", req.subject_id.clone().into());
            set(obj, "subject_id", req.subject_id.clone().into());
            set(obj, "nodeId", req.node_id.clone().into());
            set(obj, "node_id", req.node_id.clone().into());
            set(obj, "resourceKind", req.resource_kind.clone().into());
            set(obj, "resource_kind", req.resource_kind.clone().into());
            set(obj, "resourceRef", req.resource_ref.clone().into());
            set(obj, "resource_ref", req.resource_ref.clone().into());
            set(obj, "quota", req.quota.clone().into());
        }
        tentaflow_protocol::MlStudioPayload::ResourceGrantCreateResponse(resp) => {
            set(obj, "variant", "MlStudioResourceGrantCreateResponse".into());
            set(obj, "grant", ml_studio_grant_to_js(&resp.grant).into());
        }
        tentaflow_protocol::MlStudioPayload::ResourceGrantsListRequest(_) => {
            set(obj, "variant", "MlStudioResourceGrantsListRequest".into());
        }
        tentaflow_protocol::MlStudioPayload::ResourceGrantsListResponse(resp) => {
            set(obj, "variant", "MlStudioResourceGrantsListResponse".into());
            let arr = js_sys::Array::new();
            for g in &resp.grants {
                arr.push(&ml_studio_grant_to_js(g));
            }
            set(obj, "grants", arr.into());
        }
        tentaflow_protocol::MlStudioPayload::ResourceGrantRevokeRequest(req) => {
            set(obj, "variant", "MlStudioResourceGrantRevokeRequest".into());
            set(obj, "grantId", req.grant_id.clone().into());
            set(obj, "grant_id", req.grant_id.clone().into());
        }
        tentaflow_protocol::MlStudioPayload::ResourceGrantRevokeResponse(resp) => {
            set(obj, "variant", "MlStudioResourceGrantRevokeResponse".into());
            set(obj, "grantId", resp.grant_id.clone().into());
            set(obj, "grant_id", resp.grant_id.clone().into());
            set(obj, "revoked", resp.revoked.into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectResourcesRequest(req) => {
            set(obj, "variant", "MlStudioProjectResourcesRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.clone().into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectResourcesResponse(resp) => {
            set(obj, "variant", "MlStudioProjectResourcesResponse".into());
            let arr = js_sys::Array::new();
            for g in &resp.grants {
                arr.push(&ml_studio_grant_to_js(g));
            }
            set(obj, "grants", arr.into());
        }
        tentaflow_protocol::MlStudioPayload::TrainingRunsListRequest(req) => {
            set(obj, "variant", "MlStudioTrainingRunsListRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.clone().into());
        }
        tentaflow_protocol::MlStudioPayload::TrainingRunsListResponse(resp) => {
            set(obj, "variant", "MlStudioTrainingRunsListResponse".into());
            let arr = js_sys::Array::new();
            for r in &resp.runs {
                arr.push(&ml_studio_training_run_to_js(r));
            }
            set(obj, "runs", arr.into());
        }
        tentaflow_protocol::MlStudioPayload::JobsOverviewRequest(_req) => {
            set(obj, "variant", "MlStudioJobsOverviewRequest".into());
        }
        tentaflow_protocol::MlStudioPayload::JobsOverviewResponse(resp) => {
            set(obj, "variant", "MlStudioJobsOverviewResponse".into());
            let arr = js_sys::Array::new();
            for j in &resp.jobs {
                arr.push(&ml_studio_job_info_to_js(j));
            }
            set(obj, "jobs", arr.into());
            set(obj, "gpu", ml_studio_gpu_stats_to_js(&resp.gpu).into());
        }
        tentaflow_protocol::MlStudioPayload::ModelsListRequest(req) => {
            set(obj, "variant", "MlStudioModelsListRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.clone().into());
        }
        tentaflow_protocol::MlStudioPayload::ModelsListResponse(resp) => {
            set(obj, "variant", "MlStudioModelsListResponse".into());
            let arr = js_sys::Array::new();
            for m in &resp.models {
                arr.push(&ml_studio_model_to_js(m));
            }
            set(obj, "models", arr.into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectGrantsListRequest(req) => {
            set(obj, "variant", "MlStudioProjectGrantsListRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.clone().into());
        }
        tentaflow_protocol::MlStudioPayload::ProjectGrantsListResponse(resp) => {
            set(obj, "variant", "MlStudioProjectGrantsListResponse".into());
            let arr = js_sys::Array::new();
            for g in &resp.grants {
                arr.push(&ml_studio_grant_to_js(g));
            }
            set(obj, "grants", arr.into());
        }
        tentaflow_protocol::MlStudioPayload::FtTrainStartRequest(req) => {
            set(obj, "variant", "MlStudioFtTrainStartRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "datasetId", req.dataset_id.clone().into());
            set(obj, "dataset_id", req.dataset_id.into());
            set(obj, "baseModel", req.base_model.clone().into());
            set(obj, "base_model", req.base_model.into());
            set(obj, "method", req.method.into());
            set(obj, "objective", req.objective.into());
            set(obj, "mergeAdapter", req.merge_adapter.into());
            set(obj, "merge_adapter", req.merge_adapter.into());
        }
        tentaflow_protocol::MlStudioPayload::FtTrainStartResponse(resp) => {
            set(obj, "variant", "MlStudioFtTrainStartResponse".into());
            set(obj, "runId", resp.run_id.clone().into());
            set(obj, "run_id", resp.run_id.into());
            set(obj, "status", resp.status.into());
        }
        tentaflow_protocol::MlStudioPayload::DistillGenerateRequest(_) => {
            set(obj, "variant", "MlStudioDistillGenerateRequest".into());
        }
        tentaflow_protocol::MlStudioPayload::DistillGenerateResponse(resp) => {
            set(obj, "variant", "MlStudioDistillGenerateResponse".into());
            set(obj, "datasetId", resp.dataset_id.clone().into());
            set(obj, "dataset_id", resp.dataset_id.into());
            set(obj, "status", resp.status.into());
        }
        tentaflow_protocol::MlStudioPayload::DistillGenerateStatusRequest(req) => {
            set(obj, "variant", "MlStudioDistillGenerateStatusRequest".into());
            set(obj, "datasetId", req.dataset_id.clone().into());
            set(obj, "dataset_id", req.dataset_id.into());
        }
        tentaflow_protocol::MlStudioPayload::DistillGenerateStatusResponse(resp) => {
            set(obj, "variant", "MlStudioDistillGenerateStatusResponse".into());
            set(obj, "status", resp.status.into());
            set(obj, "total", (resp.total as f64).into());
            set(obj, "done", (resp.done as f64).into());
            if let Some(err) = resp.error {
                set(obj, "error", err.into());
            }
            let arr = js_sys::Array::new();
            for pair in &resp.samples {
                let o = js_sys::Object::new();
                set(&o, "question", pair.question.clone().into());
                set(&o, "answer", pair.answer.clone().into());
                set(
                    &o,
                    "rejected",
                    pair.rejected.clone().map(JsValue::from).unwrap_or(JsValue::NULL),
                );
                arr.push(&JsValue::from(o));
            }
            set(obj, "samples", arr.into());
        }
        tentaflow_protocol::MlStudioPayload::DatasetRowsRequest(req) => {
            set(obj, "variant", "MlStudioDatasetRowsRequest".into());
            set(obj, "datasetId", req.dataset_id.clone().into());
            set(obj, "dataset_id", req.dataset_id.into());
            set(obj, "limit", req.limit.unwrap_or(0).into());
        }
        tentaflow_protocol::MlStudioPayload::DatasetRowsSaveRequest(req) => {
            set(obj, "variant", "MlStudioDatasetRowsSaveRequest".into());
            set(obj, "datasetId", req.dataset_id.clone().into());
            set(obj, "dataset_id", req.dataset_id.into());
            let arr = js_sys::Array::new();
            for r in req.rows {
                arr.push(&JsValue::from(r));
            }
            set(obj, "rows", arr.into());
        }
        tentaflow_protocol::MlStudioPayload::DatasetRowsResponse(resp) => {
            set(obj, "variant", "MlStudioDatasetRowsResponse".into());
            set(obj, "datasetId", resp.dataset_id.clone().into());
            set(obj, "dataset_id", resp.dataset_id.into());
            set(obj, "kind", resp.kind.into());
            set(obj, "total", resp.total.into());
            let arr = js_sys::Array::new();
            for r in resp.rows {
                arr.push(&JsValue::from(r));
            }
            set(obj, "rows", arr.into());
            set(
                obj,
                "meta",
                resp.meta.map(JsValue::from).unwrap_or(JsValue::NULL),
            );
            set(obj, "pending", resp.pending.into());
        }
        tentaflow_protocol::MlStudioPayload::DatasetRowsSaveResponse(resp) => {
            set(obj, "variant", "MlStudioDatasetRowsSaveResponse".into());
            set(obj, "datasetId", resp.dataset_id.clone().into());
            set(obj, "dataset_id", resp.dataset_id.into());
            set(obj, "rowCount", resp.row_count.into());
            set(obj, "row_count", resp.row_count.into());
        }
        tentaflow_protocol::MlStudioPayload::FtTrainStatusRequest(req) => {
            set(obj, "variant", "MlStudioFtTrainStatusRequest".into());
            set(obj, "runId", req.run_id.clone().into());
            set(obj, "run_id", req.run_id.into());
        }
        tentaflow_protocol::MlStudioPayload::FtTrainStatusResponse(resp) => {
            set(obj, "variant", "MlStudioFtTrainStatusResponse".into());
            set(obj, "runId", resp.run_id.clone().into());
            set(obj, "run_id", resp.run_id.into());
            set(obj, "status", resp.status.into());
            set(obj, "step", (resp.step as f64).into());
            set(obj, "totalSteps", (resp.total_steps as f64).into());
            set(obj, "total_steps", (resp.total_steps as f64).into());
            set(obj, "trainLoss", opt_f64_to_js(resp.train_loss));
            set(obj, "train_loss", opt_f64_to_js(resp.train_loss));
            set(obj, "evalLoss", opt_f64_to_js(resp.eval_loss));
            set(obj, "eval_loss", opt_f64_to_js(resp.eval_loss));
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
            set(obj, "lossCurve", ml_studio_loss_curve_to_js(&resp.loss_curve).into());
            set(obj, "loss_curve", ml_studio_loss_curve_to_js(&resp.loss_curve).into());
            match resp.sync_phase {
                Some(p) => {
                    set(obj, "syncPhase", p.clone().into());
                    set(obj, "sync_phase", p.into());
                }
                None => {
                    set(obj, "syncPhase", JsValue::NULL);
                    set(obj, "sync_phase", JsValue::NULL);
                }
            }
            set(obj, "syncBytesSent", (resp.sync_bytes_sent as f64).into());
            set(obj, "sync_bytes_sent", (resp.sync_bytes_sent as f64).into());
            set(obj, "syncBytesTotal", (resp.sync_bytes_total as f64).into());
            set(obj, "sync_bytes_total", (resp.sync_bytes_total as f64).into());
            set(obj, "syncRateBps", (resp.sync_rate_bps as f64).into());
            set(obj, "sync_rate_bps", (resp.sync_rate_bps as f64).into());
        }
        tentaflow_protocol::MlStudioPayload::FtExportRequest(req) => {
            set(obj, "variant", "MlStudioFtExportRequest".into());
            set(obj, "modelId", req.model_id.clone().into());
            set(obj, "model_id", req.model_id.into());
            set(obj, "outtype", req.outtype.into());
        }
        tentaflow_protocol::MlStudioPayload::FtExportResponse(resp) => {
            set(obj, "variant", "MlStudioFtExportResponse".into());
            set(obj, "modelId", resp.model_id.clone().into());
            set(obj, "model_id", resp.model_id.into());
            set(obj, "status", resp.status.into());
        }
        tentaflow_protocol::MlStudioPayload::FtExportStatusRequest(req) => {
            set(obj, "variant", "MlStudioFtExportStatusRequest".into());
            set(obj, "modelId", req.model_id.clone().into());
            set(obj, "model_id", req.model_id.into());
        }
        tentaflow_protocol::MlStudioPayload::FtExportStatusResponse(resp) => {
            set(obj, "variant", "MlStudioFtExportStatusResponse".into());
            set(obj, "modelId", resp.model_id.clone().into());
            set(obj, "model_id", resp.model_id.into());
            set(obj, "status", resp.status.into());
            match resp.gguf_path {
                Some(p) => {
                    set(obj, "ggufPath", p.clone().into());
                    set(obj, "gguf_path", p.into());
                }
                None => {
                    set(obj, "ggufPath", JsValue::NULL);
                    set(obj, "gguf_path", JsValue::NULL);
                }
            }
            match resp.size_bytes {
                Some(s) => {
                    set(obj, "sizeBytes", (s as f64).into());
                    set(obj, "size_bytes", (s as f64).into());
                }
                None => {
                    set(obj, "sizeBytes", JsValue::NULL);
                    set(obj, "size_bytes", JsValue::NULL);
                }
            }
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
        }
        tentaflow_protocol::MlStudioPayload::FtDeployRequest(req) => {
            set(obj, "variant", "MlStudioFtDeployRequest".into());
            set(obj, "modelId", req.model_id.clone().into());
            set(obj, "model_id", req.model_id.into());
        }
        tentaflow_protocol::MlStudioPayload::FtDeployResponse(resp) => {
            set(obj, "variant", "MlStudioFtDeployResponse".into());
            set(obj, "modelId", resp.model_id.clone().into());
            set(obj, "model_id", resp.model_id.into());
            set(obj, "modelName", resp.model_name.clone().into());
            set(obj, "model_name", resp.model_name.into());
            set(obj, "status", resp.status.into());
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
        }
        tentaflow_protocol::MlStudioPayload::FtChatRequest(req) => {
            set(obj, "variant", "MlStudioFtChatRequest".into());
            set(obj, "modelId", req.model_id.clone().into());
            set(obj, "model_id", req.model_id.into());
            set(obj, "message", req.message.into());
            set(obj, "maxTokens", req.max_tokens.into());
            set(obj, "max_tokens", req.max_tokens.into());
        }
        tentaflow_protocol::MlStudioPayload::FtChatResponse(resp) => {
            set(obj, "variant", "MlStudioFtChatResponse".into());
            set(obj, "answer", resp.answer.into());
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
        }
        tentaflow_protocol::MlStudioPayload::RecogTrainStartRequest(req) => {
            set(obj, "variant", "MlStudioRecogTrainStartRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "datasetId", req.dataset_id.clone().into());
            set(obj, "dataset_id", req.dataset_id.into());
            set(obj, "variant_name", req.variant.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogTrainStartResponse(resp) => {
            set(obj, "variant", "MlStudioRecogTrainStartResponse".into());
            set(obj, "runId", resp.run_id.clone().into());
            set(obj, "run_id", resp.run_id.into());
            set(obj, "status", resp.status.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogTrainStatusRequest(req) => {
            set(obj, "variant", "MlStudioRecogTrainStatusRequest".into());
            set(obj, "runId", req.run_id.clone().into());
            set(obj, "run_id", req.run_id.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogTrainStatusResponse(resp) => {
            set(obj, "variant", "MlStudioRecogTrainStatusResponse".into());
            set(obj, "runId", resp.run_id.clone().into());
            set(obj, "run_id", resp.run_id.into());
            set(obj, "status", resp.status.into());
            set(obj, "epoch", (resp.epoch as f64).into());
            set(obj, "totalEpochs", (resp.total_epochs as f64).into());
            set(obj, "total_epochs", (resp.total_epochs as f64).into());
            set(obj, "trainLoss", opt_f64_to_js(resp.train_loss));
            set(obj, "train_loss", opt_f64_to_js(resp.train_loss));
            set(obj, "map50", opt_f64_to_js(resp.map50));
            set(obj, "map50_95", opt_f64_to_js(resp.map50_95));
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
            set(obj, "curve", ml_studio_recog_curve_to_js(&resp.curve).into());
            match resp.sync_phase {
                Some(p) => {
                    set(obj, "syncPhase", p.clone().into());
                    set(obj, "sync_phase", p.into());
                }
                None => {
                    set(obj, "syncPhase", JsValue::NULL);
                    set(obj, "sync_phase", JsValue::NULL);
                }
            }
            set(obj, "syncBytesSent", (resp.sync_bytes_sent as f64).into());
            set(obj, "sync_bytes_sent", (resp.sync_bytes_sent as f64).into());
            set(obj, "syncBytesTotal", (resp.sync_bytes_total as f64).into());
            set(obj, "sync_bytes_total", (resp.sync_bytes_total as f64).into());
            set(obj, "syncRateBps", (resp.sync_rate_bps as f64).into());
            set(obj, "sync_rate_bps", (resp.sync_rate_bps as f64).into());
            set(obj, "etaS", (resp.eta_s as f64).into());
            set(obj, "eta_s", (resp.eta_s as f64).into());
            set(obj, "elapsedS", (resp.elapsed_s as f64).into());
            set(obj, "elapsed_s", (resp.elapsed_s as f64).into());
            set(obj, "gpuMemMb", (resp.gpu_mem_mb as f64).into());
            set(obj, "gpu_mem_mb", (resp.gpu_mem_mb as f64).into());
            set(obj, "stage", resp.stage.into());
        }
        tentaflow_protocol::MlStudioPayload::ClassifierTrainStartRequest(req) => {
            set(obj, "variant", "MlStudioClassifierTrainStartRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "datasetId", req.dataset_id.clone().into());
            set(obj, "dataset_id", req.dataset_id.into());
            set(obj, "attribute", req.attribute.into());
            set(obj, "sourceClass", req.source_class.clone().into());
            set(obj, "source_class", req.source_class.into());
            set(obj, "variant_name", req.variant.into());
        }
        tentaflow_protocol::MlStudioPayload::ClassifierTrainStartResponse(resp) => {
            set(obj, "variant", "MlStudioClassifierTrainStartResponse".into());
            set(obj, "runId", resp.run_id.clone().into());
            set(obj, "run_id", resp.run_id.into());
            set(obj, "status", resp.status.into());
        }
        tentaflow_protocol::MlStudioPayload::GenericTrainStatusRequest(req) => {
            set(obj, "variant", "MlStudioGenericTrainStatusRequest".into());
            set(obj, "runId", req.run_id.clone().into());
            set(obj, "run_id", req.run_id.into());
        }
        tentaflow_protocol::MlStudioPayload::GenericTrainStatusResponse(resp) => {
            set(obj, "variant", "MlStudioGenericTrainStatusResponse".into());
            set(obj, "runId", resp.run_id.clone().into());
            set(obj, "run_id", resp.run_id.into());
            set(obj, "status", resp.status.into());
            set(obj, "epoch", (resp.epoch as f64).into());
            set(obj, "totalEpochs", (resp.total_epochs as f64).into());
            set(obj, "total_epochs", (resp.total_epochs as f64).into());
            set(obj, "curve", ml_studio_generic_curve_to_js(&resp.curve).into());
            set(obj, "error", resp.error.into());
            match resp.sync_phase {
                Some(p) => {
                    set(obj, "syncPhase", p.clone().into());
                    set(obj, "sync_phase", p.into());
                }
                None => {
                    set(obj, "syncPhase", JsValue::NULL);
                    set(obj, "sync_phase", JsValue::NULL);
                }
            }
            set(obj, "syncBytesSent", (resp.sync_bytes_sent as f64).into());
            set(obj, "sync_bytes_sent", (resp.sync_bytes_sent as f64).into());
            set(obj, "syncBytesTotal", (resp.sync_bytes_total as f64).into());
            set(obj, "sync_bytes_total", (resp.sync_bytes_total as f64).into());
            set(obj, "syncRateBps", (resp.sync_rate_bps as f64).into());
            set(obj, "sync_rate_bps", (resp.sync_rate_bps as f64).into());
            set(obj, "etaS", (resp.eta_s as f64).into());
            set(obj, "eta_s", (resp.eta_s as f64).into());
            set(obj, "elapsedS", (resp.elapsed_s as f64).into());
            set(obj, "elapsed_s", (resp.elapsed_s as f64).into());
            set(obj, "gpuMemMb", (resp.gpu_mem_mb as f64).into());
            set(obj, "gpu_mem_mb", (resp.gpu_mem_mb as f64).into());
            set(obj, "stage", resp.stage.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogDatasetRegisterRequest(req) => {
            set(obj, "variant", "MlStudioRecogDatasetRegisterRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "name", req.name.into());
            set(obj, "path", req.path.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogDatasetRegisterResponse(resp) => {
            set(obj, "variant", "MlStudioRecogDatasetRegisterResponse".into());
            set(obj, "dataset", ml_studio_dataset_summary_to_js(&resp.dataset).into());
        }
        tentaflow_protocol::MlStudioPayload::RecogStageMediaRequest(req) => {
            set(obj, "variant", "MlStudioRecogStageMediaRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "filename", req.filename.into());
            set(obj, "uploadId", req.upload_id.clone().into());
            set(obj, "upload_id", req.upload_id.into());
            set(obj, "seq", req.seq.into());
            set(obj, "totalChunks", req.total_chunks.into());
            set(obj, "total_chunks", req.total_chunks.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogStageMediaResponse(resp) => {
            set(obj, "variant", "MlStudioRecogStageMediaResponse".into());
            set(obj, "uploadId", resp.upload_id.clone().into());
            set(obj, "upload_id", resp.upload_id.into());
            set(obj, "receivedChunks", resp.received_chunks.into());
            set(obj, "received_chunks", resp.received_chunks.into());
            set(obj, "receivedBytes", (resp.received_bytes as f64).into());
            set(obj, "received_bytes", (resp.received_bytes as f64).into());
            set(obj, "staged", resp.staged.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogBuildDatasetRequest(req) => {
            set(obj, "variant", "MlStudioRecogBuildDatasetRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "datasetName", req.dataset_name.clone().into());
            set(obj, "dataset_name", req.dataset_name.into());
            set(obj, "fps", req.fps.into());
            match req.source_dir {
                Some(s) => {
                    set(obj, "sourceDir", s.clone().into());
                    set(obj, "source_dir", s.into());
                }
                None => {
                    set(obj, "sourceDir", JsValue::NULL);
                    set(obj, "source_dir", JsValue::NULL);
                }
            }
        }
        tentaflow_protocol::MlStudioPayload::RecogBuildDatasetResponse(resp) => {
            set(obj, "variant", "MlStudioRecogBuildDatasetResponse".into());
            set(obj, "buildId", resp.build_id.clone().into());
            set(obj, "build_id", resp.build_id.into());
            set(obj, "status", resp.status.into());
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
        }
        tentaflow_protocol::MlStudioPayload::RecogBuildStatusRequest(req) => {
            set(obj, "variant", "MlStudioRecogBuildStatusRequest".into());
            set(obj, "buildId", req.build_id.clone().into());
            set(obj, "build_id", req.build_id.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogBuildStatusResponse(resp) => {
            set(obj, "variant", "MlStudioRecogBuildStatusResponse".into());
            set(obj, "buildId", resp.build_id.clone().into());
            set(obj, "build_id", resp.build_id.into());
            set(obj, "status", resp.status.into());
            set(obj, "filesTotal", (resp.files_total as f64).into());
            set(obj, "files_total", (resp.files_total as f64).into());
            set(obj, "filesDone", (resp.files_done as f64).into());
            set(obj, "files_done", (resp.files_done as f64).into());
            set(obj, "framesExtracted", (resp.frames_extracted as f64).into());
            set(obj, "frames_extracted", (resp.frames_extracted as f64).into());
            if let Some(ds) = &resp.dataset {
                set(obj, "dataset", ml_studio_dataset_summary_to_js(ds).into());
            }
            set(obj, "imageCount", (resp.image_count as f64).into());
            set(obj, "image_count", (resp.image_count as f64).into());
            set(obj, "categoryCount", resp.category_count.into());
            set(obj, "category_count", resp.category_count.into());
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
        }
        tentaflow_protocol::MlStudioPayload::RecogAutolabelRequest(req) => {
            set(obj, "variant", "MlStudioRecogAutolabelRequest".into());
            set(obj, "datasetId", req.dataset_id.clone().into());
            set(obj, "dataset_id", req.dataset_id.into());
            set(obj, "threshold", req.threshold.into());
            set(obj, "mode", req.mode.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogAutolabelResponse(resp) => {
            set(obj, "variant", "MlStudioRecogAutolabelResponse".into());
            set(obj, "jobId", resp.job_id.clone().into());
            set(obj, "job_id", resp.job_id.into());
            set(obj, "status", resp.status.into());
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
        }
        tentaflow_protocol::MlStudioPayload::RecogAutolabelStatusRequest(req) => {
            set(obj, "variant", "MlStudioRecogAutolabelStatusRequest".into());
            set(obj, "jobId", req.job_id.clone().into());
            set(obj, "job_id", req.job_id.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogAutolabelStatusResponse(resp) => {
            set(obj, "variant", "MlStudioRecogAutolabelStatusResponse".into());
            set(obj, "status", resp.status.into());
            set(obj, "imagesTotal", (resp.images_total as f64).into());
            set(obj, "images_total", (resp.images_total as f64).into());
            set(obj, "imagesDone", (resp.images_done as f64).into());
            set(obj, "images_done", (resp.images_done as f64).into());
            set(obj, "detections", (resp.detections as f64).into());
            set(obj, "skippedUnknown", (resp.skipped_unknown as f64).into());
            set(obj, "skipped_unknown", (resp.skipped_unknown as f64).into());
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
        }
        tentaflow_protocol::MlStudioPayload::RecogDetectRequest(req) => {
            set(obj, "variant", "MlStudioRecogDetectRequest".into());
            set(obj, "modelId", req.model_id.clone().into());
            set(obj, "model_id", req.model_id.into());
            set(obj, "threshold", req.threshold.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogDetectResponse(resp) => {
            set(obj, "variant", "MlStudioRecogDetectResponse".into());
            set(obj, "detectionsJson", resp.detections_json.clone().into());
            set(obj, "detections_json", resp.detections_json.into());
            set(obj, "width", (resp.width as f64).into());
            set(obj, "height", (resp.height as f64).into());
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
        }
        tentaflow_protocol::MlStudioPayload::RecogImagesListRequest(req) => {
            set(obj, "variant", "MlStudioRecogImagesListRequest".into());
            set(obj, "datasetId", req.dataset_id.clone().into());
            set(obj, "dataset_id", req.dataset_id.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogImagesListResponse(resp) => {
            set(obj, "variant", "MlStudioRecogImagesListResponse".into());
            set(obj, "imagesJson", resp.images_json.clone().into());
            set(obj, "images_json", resp.images_json.into());
            set(obj, "categoriesJson", resp.categories_json.clone().into());
            set(obj, "categories_json", resp.categories_json.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogImageRequest(req) => {
            set(obj, "variant", "MlStudioRecogImageRequest".into());
            set(obj, "datasetId", req.dataset_id.clone().into());
            set(obj, "dataset_id", req.dataset_id.into());
            set(obj, "imageId", req.image_id.clone().into());
            set(obj, "image_id", req.image_id.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogImageResponse(resp) => {
            set(obj, "variant", "MlStudioRecogImageResponse".into());
            set(obj, "imageB64", resp.image_b64.clone().into());
            set(obj, "image_b64", resp.image_b64.into());
            set(obj, "mime", resp.mime.into());
            set(obj, "origWidth", (resp.orig_width as f64).into());
            set(obj, "orig_width", (resp.orig_width as f64).into());
            set(obj, "origHeight", (resp.orig_height as f64).into());
            set(obj, "orig_height", (resp.orig_height as f64).into());
            set(obj, "annotationsJson", resp.annotations_json.clone().into());
            set(obj, "annotations_json", resp.annotations_json.into());
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
        }
        tentaflow_protocol::MlStudioPayload::RecogSaveAnnotationsRequest(req) => {
            set(obj, "variant", "MlStudioRecogSaveAnnotationsRequest".into());
            set(obj, "datasetId", req.dataset_id.clone().into());
            set(obj, "dataset_id", req.dataset_id.into());
            set(obj, "imageId", req.image_id.clone().into());
            set(obj, "image_id", req.image_id.into());
            set(obj, "approve", req.approve.into());
        }
        tentaflow_protocol::MlStudioPayload::RecogSaveAnnotationsResponse(resp) => {
            set(obj, "variant", "MlStudioRecogSaveAnnotationsResponse".into());
            set(obj, "ok", resp.ok.into());
            match resp.error {
                Some(e) => set(obj, "error", e.into()),
                None => set(obj, "error", JsValue::NULL),
            }
        }
        tentaflow_protocol::MlStudioPayload::SchemaGetRequest(req) => {
            set(obj, "variant", "MlStudioSchemaGetRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
        }
        tentaflow_protocol::MlStudioPayload::SchemaGetResponse(resp) => {
            set(obj, "variant", "MlStudioSchemaGetResponse".into());
            set(obj, "schemaJson", resp.schema_json.clone().into());
            set(obj, "schema_json", resp.schema_json.into());
        }
        tentaflow_protocol::MlStudioPayload::SchemaSaveRequest(req) => {
            set(obj, "variant", "MlStudioSchemaSaveRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "schemaJson", req.schema_json.clone().into());
            set(obj, "schema_json", req.schema_json.into());
        }
        tentaflow_protocol::MlStudioPayload::SchemaSaveResponse(resp) => {
            set(obj, "variant", "MlStudioSchemaSaveResponse".into());
            set(obj, "ok", resp.ok.into());
        }
        tentaflow_protocol::MlStudioPayload::LookupDictsListRequest(req) => {
            set(obj, "variant", "MlStudioLookupDictsListRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
        }
        tentaflow_protocol::MlStudioPayload::LookupDictsListResponse(resp) => {
            set(obj, "variant", "MlStudioLookupDictsListResponse".into());
            set(obj, "dictsJson", resp.dicts_json.clone().into());
            set(obj, "dicts_json", resp.dicts_json.into());
        }
        tentaflow_protocol::MlStudioPayload::LookupDictSaveRequest(req) => {
            set(obj, "variant", "MlStudioLookupDictSaveRequest".into());
            set(obj, "projectId", req.project_id.clone().into());
            set(obj, "project_id", req.project_id.into());
            set(obj, "dictId", req.dict_id.clone().into());
            set(obj, "dict_id", req.dict_id.into());
            set(obj, "name", req.name.into());
            set(obj, "rowsJson", req.rows_json.clone().into());
            set(obj, "rows_json", req.rows_json.into());
        }
        tentaflow_protocol::MlStudioPayload::LookupDictSaveResponse(resp) => {
            set(obj, "variant", "MlStudioLookupDictSaveResponse".into());
            set(obj, "dictId", resp.dict_id.clone().into());
            set(obj, "dict_id", resp.dict_id.into());
        }
        tentaflow_protocol::MlStudioPayload::LookupDictDeleteRequest(req) => {
            set(obj, "variant", "MlStudioLookupDictDeleteRequest".into());
            set(obj, "dictId", req.dict_id.clone().into());
            set(obj, "dict_id", req.dict_id.into());
        }
        tentaflow_protocol::MlStudioPayload::LookupDictDeleteResponse(resp) => {
            set(obj, "variant", "MlStudioLookupDictDeleteResponse".into());
            set(obj, "ok", resp.ok.into());
        }
        tentaflow_protocol::MlStudioPayload::ServiceModelsListRequest(req) => {
            set(obj, "variant", "MlStudioServiceModelsListRequest".into());
            set(obj, "capability", req.capability.into());
        }
        tentaflow_protocol::MlStudioPayload::ServiceModelsListResponse(resp) => {
            set(obj, "variant", "MlStudioServiceModelsListResponse".into());
            set(obj, "modelsJson", resp.models_json.clone().into());
            set(obj, "models_json", resp.models_json.into());
        }
    }
}

/// Mapuje krzywą treningu detekcji (punkty per epoka) na tablicę JS.
fn ml_studio_recog_curve_to_js(
    points: &[tentaflow_protocol::MlStudioRecogMetricPoint],
) -> js_sys::Array {
    let arr = js_sys::Array::new();
    for p in points {
        let item = js_sys::Object::new();
        set(&item, "epoch", (p.epoch as f64).into());
        set(&item, "trainLoss", opt_f64_to_js(p.train_loss));
        set(&item, "train_loss", opt_f64_to_js(p.train_loss));
        set(&item, "map50", opt_f64_to_js(p.map50));
        arr.push(&item);
    }
    arr
}

/// Mapuje generyczną krzywą treningu (punkty {epoch, metric_name, value}) na
/// tablicę JS. Używana przez status klasyfikatora atrybutu i inne tory generyczne.
fn ml_studio_generic_curve_to_js(
    points: &[tentaflow_protocol::GenericMetricPoint],
) -> js_sys::Array {
    let arr = js_sys::Array::new();
    for p in points {
        let item = js_sys::Object::new();
        set(&item, "epoch", (p.epoch as f64).into());
        set(&item, "metricName", p.metric_name.clone().into());
        set(&item, "metric_name", p.metric_name.clone().into());
        set(&item, "value", (p.value as f64).into());
        arr.push(&item);
    }
    arr
}

/// Mapuje krzywą straty (lista punktów per krok) na tablicę JS dla wykresu f02.
fn ml_studio_loss_curve_to_js(
    points: &[tentaflow_protocol::MlStudioLossPoint],
) -> js_sys::Array {
    let arr = js_sys::Array::new();
    for p in points {
        let item = js_sys::Object::new();
        set(&item, "step", (p.step as f64).into());
        set(&item, "trainLoss", opt_f64_to_js(p.train_loss));
        set(&item, "train_loss", opt_f64_to_js(p.train_loss));
        set(&item, "evalLoss", opt_f64_to_js(p.eval_loss));
        set(&item, "eval_loss", opt_f64_to_js(p.eval_loss));
        arr.push(&item);
    }
    arr
}

fn localized_texts_to_js(items: Vec<tentaflow_protocol::ComplianceLocalizedText>) -> js_sys::Array {
    let arr = js_sys::Array::new();
    for item in items {
        let obj = js_sys::Object::new();
        set(&obj, "locale", item.locale.into());
        set(&obj, "text", item.text.into());
        arr.push(&obj);
    }
    arr
}

fn compliance_admin_payload_to_js(
    obj: &js_sys::Object,
    payload: tentaflow_protocol::ComplianceAdminPayload,
) {
    use tentaflow_protocol::ComplianceAdminPayload as P;
    match payload {
        P::ListDataCategoriesRequest => {
            set(obj, "variant", "ComplianceDataCategoriesListRequest".into());
            set(
                obj,
                "warning",
                "unexpected_request_variant_in_response".into(),
            );
        }
        P::ListDataCategoriesResponse { categories } => {
            set(
                obj,
                "variant",
                "ComplianceDataCategoriesListResponse".into(),
            );
            let arr = js_sys::Array::new();
            for category in categories {
                let item = js_sys::Object::new();
                set(&item, "categoryId", category.category_id.clone().into());
                set(&item, "category_id", category.category_id.into());
                set(&item, "slug", category.slug.into());
                set(
                    &item,
                    "nameTranslations",
                    localized_texts_to_js(category.name_translations).into(),
                );
                set(
                    &item,
                    "descriptionTranslations",
                    localized_texts_to_js(category.description_translations).into(),
                );
                set(&item, "personalData", category.personal_data.into());
                set(&item, "personal_data", category.personal_data.into());
                set(&item, "sensitiveData", category.sensitive_data.into());
                set(&item, "sensitive_data", category.sensitive_data.into());
                set(&item, "riskClass", category.risk_class.clone().into());
                set(&item, "risk_class", category.risk_class.into());
                set(&item, "sourceScope", category.source_scope.clone().into());
                set(&item, "source_scope", category.source_scope.into());
                if let Some(addon_id) = category.addon_id {
                    set(&item, "addonId", addon_id.clone().into());
                    set(&item, "addon_id", addon_id.into());
                }
                arr.push(&item);
            }
            set(obj, "categories", arr.into());
        }
        P::ListRetentionPoliciesRequest => {
            set(
                obj,
                "variant",
                "ComplianceRetentionPoliciesListRequest".into(),
            );
            set(
                obj,
                "warning",
                "unexpected_request_variant_in_response".into(),
            );
        }
        P::ListRetentionPoliciesResponse { policies } => {
            set(
                obj,
                "variant",
                "ComplianceRetentionPoliciesListResponse".into(),
            );
            let arr = js_sys::Array::new();
            for policy in policies {
                let item = js_sys::Object::new();
                set(
                    &item,
                    "retentionPolicyId",
                    policy.retention_policy_id.clone().into(),
                );
                set(
                    &item,
                    "retention_policy_id",
                    policy.retention_policy_id.into(),
                );
                set(&item, "slug", policy.slug.into());
                set(
                    &item,
                    "nameTranslations",
                    localized_texts_to_js(policy.name_translations).into(),
                );
                set(&item, "scopeKind", policy.scope_kind.clone().into());
                set(&item, "scope_kind", policy.scope_kind.into());
                if let Some(category_id) = policy.category_id {
                    set(&item, "categoryId", category_id.clone().into());
                    set(&item, "category_id", category_id.into());
                }
                set(
                    &item,
                    "retentionDays",
                    policy.retention_days.clone().into(),
                );
                set(
                    &item,
                    "retention_days",
                    policy.retention_days.clone().into(),
                );
                set(&item, "minimumDays", policy.minimum_days.clone().into());
                set(&item, "minimum_days", policy.minimum_days.clone().into());
                set(
                    &item,
                    "actionAfterRetention",
                    policy.action_after_retention.clone().into(),
                );
                set(
                    &item,
                    "action_after_retention",
                    policy.action_after_retention.into(),
                );
                set(&item, "isDefault", policy.is_default.into());
                set(&item, "is_default", policy.is_default.into());
                set(&item, "isActive", policy.is_active.into());
                set(&item, "is_active", policy.is_active.into());
                arr.push(&item);
            }
            set(obj, "policies", arr.into());
        }
        P::ListAiEventsRequest(filter) => {
            set(obj, "variant", "ComplianceAiEventsListRequest".into());
            if let Some(status) = filter.status {
                set(obj, "status", status.into());
            }
            if let Some(user_id) = filter.user_id {
                set(obj, "userId", user_id.clone().into());
                set(obj, "user_id", user_id.clone().into());
            }
            if let Some(addon_id) = filter.addon_id {
                set(obj, "addonId", addon_id.clone().into());
                set(obj, "addon_id", addon_id.into());
            }
            if let Some(limit) = filter.limit {
                set(obj, "limit", limit.into());
            }
            if let Some(offset) = filter.offset {
                set(obj, "offset", offset.into());
            }
            set(
                obj,
                "warning",
                "unexpected_request_variant_in_response".into(),
            );
        }
        P::ListAiEventsResponse { events } => {
            set(obj, "variant", "ComplianceAiEventsListResponse".into());
            let arr = js_sys::Array::new();
            for event in events {
                let item = js_sys::Object::new();
                set(&item, "eventId", event.event_id.clone().into());
                set(&item, "event_id", event.event_id.into());
                if let Some(user_id) = event.user_id {
                    set(&item, "userId", user_id.clone().into());
                    set(&item, "user_id", user_id.clone().into());
                }
                set(&item, "nodeId", event.node_id.clone().into());
                set(&item, "node_id", event.node_id.into());
                if let Some(addon_id) = event.addon_id {
                    set(&item, "addonId", addon_id.clone().into());
                    set(&item, "addon_id", addon_id.into());
                }
                if let Some(instance_id) = event.instance_id {
                    set(&item, "instanceId", instance_id.clone().into());
                    set(&item, "instance_id", instance_id.into());
                }
                if let Some(flow_id) = event.flow_id {
                    set(&item, "flowId", flow_id.clone().into());
                    set(&item, "flow_id", flow_id.clone().into());
                }
                if let Some(flow_node_id) = event.flow_node_id {
                    set(&item, "flowNodeId", flow_node_id.clone().into());
                    set(&item, "flow_node_id", flow_node_id.into());
                }
                set(&item, "requestId", event.request_id.clone().into());
                set(&item, "request_id", event.request_id.into());
                set(&item, "modelId", event.model_id.clone().into());
                set(&item, "model_id", event.model_id.into());
                set(&item, "backend", event.backend.into());
                set(&item, "startedAt", event.started_at.clone().into());
                set(&item, "started_at", event.started_at.into());
                if let Some(finished_at) = event.finished_at {
                    set(&item, "finishedAt", finished_at.clone().into());
                    set(&item, "finished_at", finished_at.into());
                }
                set(&item, "status", event.status.into());
                set(&item, "riskClass", event.risk_class.clone().into());
                set(&item, "risk_class", event.risk_class.into());
                if let Some(legal_basis_id) = event.legal_basis_id {
                    set(&item, "legalBasisId", legal_basis_id.clone().into());
                    set(&item, "legal_basis_id", legal_basis_id.into());
                }
                set(
                    &item,
                    "retentionPolicyId",
                    event.retention_policy_id.clone().into(),
                );
                set(
                    &item,
                    "retention_policy_id",
                    event.retention_policy_id.into(),
                );
                set(&item, "promptHash", event.prompt_hash.clone().into());
                set(&item, "prompt_hash", event.prompt_hash.into());
                set(&item, "responseHash", event.response_hash.clone().into());
                set(&item, "response_hash", event.response_hash.into());
                if let Some(audit_log_id) = event.audit_log_id {
                    set(&item, "auditLogId", audit_log_id.clone().into());
                    set(&item, "audit_log_id", audit_log_id.clone().into());
                }
                if let Some(error_message) = event.error_message {
                    set(&item, "errorMessage", error_message.clone().into());
                    set(&item, "error_message", error_message.into());
                }
                arr.push(&item);
            }
            set(obj, "events", arr.into());
        }
    }
}

// Decoder dla `RoleCatalogPayload` — kazdy wariant payloadu wystawia camelCase
// properties z pol DTO (kompatybilne z reszta web protocol glue).
fn role_catalog_payload_to_js(
    obj: &js_sys::Object,
    payload: tentaflow_protocol::RoleCatalogPayload,
) {
    use tentaflow_protocol::RoleCatalogPayload as P;
    match payload {
        P::ListRequest(filter) => {
            set(obj, "variant", "RoleCatalogListRequest".into());
            if let Some(k) = filter.kind {
                set(obj, "kind", k.into());
            }
            if let Some(active) = filter.is_active {
                set(obj, "isActive", active.into());
            }
            if let Some(s) = filter.search {
                set(obj, "search", s.into());
            }
            if let Some(l) = filter.limit {
                set(obj, "limit", l.into());
            }
            if let Some(o) = filter.offset {
                set(obj, "offset", o.into());
            }
        }
        P::ListResponse { roles } => {
            set(obj, "variant", "RoleCatalogListResponse".into());
            let arr = js_sys::Array::new();
            for r in roles {
                arr.push(&role_catalog_summary_to_js(r).into());
            }
            set(obj, "roles", arr.into());
        }
        P::GetRequest { id } => {
            set(obj, "variant", "RoleCatalogGetRequest".into());
            set(obj, "id", id.into());
        }
        P::GetBySlugRequest { slug } => {
            set(obj, "variant", "RoleCatalogGetBySlugRequest".into());
            set(obj, "slug", slug.into());
        }
        P::GetResponse { role } => {
            set(obj, "variant", "RoleCatalogGetResponse".into());
            if let Some(r) = role {
                set(obj, "role", role_catalog_detail_to_js(r).into());
            }
        }
        P::ListLocalesRequest => {
            set(obj, "variant", "RoleCatalogListLocalesRequest".into());
        }
        P::ListLocalesResponse { locales } => {
            set(obj, "variant", "RoleCatalogListLocalesResponse".into());
            let arr = js_sys::Array::new();
            for loc in locales {
                let item = js_sys::Object::new();
                set(&item, "code", loc.code.into());
                set(&item, "displayName", loc.display_name.into());
                set(&item, "isDefault", loc.is_default.into());
                arr.push(&item.into());
            }
            set(obj, "locales", arr.into());
        }
        P::CreateRequest(req) => {
            set(obj, "variant", "RoleCatalogCreateRequest".into());
            set(obj, "slug", req.slug.into());
            set(obj, "kind", req.kind.into());
            set(
                obj,
                "nameTranslations",
                translations_vec_to_js(&req.name_translations).into(),
            );
            set(
                obj,
                "descriptionTranslations",
                translations_vec_to_js(&req.description_translations).into(),
            );
            if let Some(i) = req.icon {
                set(obj, "icon", i.into());
            }
            if let Some(c) = req.color_hint {
                set(obj, "colorHint", c.into());
            }
            set(obj, "isManager", req.is_manager.into());
            set(
                obj,
                "defaultVisibilityScope",
                req.default_visibility_scope.into(),
            );
        }
        P::CreateResponse(detail) => {
            set(obj, "variant", "RoleCatalogCreateResponse".into());
            set(obj, "role", role_catalog_detail_to_js(detail).into());
        }
        P::UpdateRequest(req) => {
            set(obj, "variant", "RoleCatalogUpdateRequest".into());
            set(obj, "id", req.id.into());
            if let Some(k) = req.kind {
                set(obj, "kind", k.into());
            }
            if let Some(nt) = req.name_translations {
                set(obj, "nameTranslations", translations_vec_to_js(&nt).into());
            }
            if let Some(dt) = req.description_translations {
                set(
                    obj,
                    "descriptionTranslations",
                    translations_vec_to_js(&dt).into(),
                );
            }
            // icon/color_hint: Option<Option<String>> — Some(None) = JS null clear,
            // Some(Some) = string. None = pole pomijane (brak modyfikacji).
            if let Some(icon_opt) = req.icon {
                match icon_opt {
                    Some(v) => set(obj, "icon", v.into()),
                    None => set(obj, "icon", JsValue::NULL),
                }
            }
            if let Some(color_opt) = req.color_hint {
                match color_opt {
                    Some(v) => set(obj, "colorHint", v.into()),
                    None => set(obj, "colorHint", JsValue::NULL),
                }
            }
            if let Some(m) = req.is_manager {
                set(obj, "isManager", m.into());
            }
            if let Some(s) = req.default_visibility_scope {
                set(obj, "defaultVisibilityScope", s.into());
            }
        }
        P::UpdateResponse(detail) => {
            set(obj, "variant", "RoleCatalogUpdateResponse".into());
            set(obj, "role", role_catalog_detail_to_js(detail).into());
        }
        P::DeactivateRequest { id } => {
            set(obj, "variant", "RoleCatalogDeactivateRequest".into());
            set(obj, "id", id.into());
        }
        P::DeactivateResponse { deactivated } => {
            set(obj, "variant", "RoleCatalogDeactivateResponse".into());
            set(obj, "deactivated", deactivated.into());
        }
    }
}

fn translations_vec_to_js(translations: &[(String, String)]) -> js_sys::Array {
    let arr = js_sys::Array::new();
    for (code, value) in translations {
        let pair = js_sys::Array::new();
        pair.push(&JsValue::from_str(code));
        pair.push(&JsValue::from_str(value));
        arr.push(&pair.into());
    }
    arr
}

fn role_catalog_summary_to_js(s: tentaflow_protocol::RoleCatalogSummary) -> js_sys::Object {
    let o = js_sys::Object::new();
    set(&o, "id", s.id.into());
    set(&o, "slug", s.slug.into());
    set(&o, "kind", s.kind.into());
    set(
        &o,
        "nameTranslations",
        translations_vec_to_js(&s.name_translations).into(),
    );
    if let Some(i) = s.icon {
        set(&o, "icon", i.into());
    }
    if let Some(c) = s.color_hint {
        set(&o, "colorHint", c.into());
    }
    set(&o, "isManager", s.is_manager.into());
    set(
        &o,
        "defaultVisibilityScope",
        s.default_visibility_scope.into(),
    );
    set(&o, "isActive", s.is_active.into());
    o
}

fn role_catalog_detail_to_js(d: tentaflow_protocol::RoleCatalogDetail) -> js_sys::Object {
    let o = js_sys::Object::new();
    set(&o, "id", d.id.into());
    set(&o, "orgId", d.org_id.into());
    set(&o, "slug", d.slug.into());
    set(&o, "kind", d.kind.into());
    set(
        &o,
        "nameTranslations",
        translations_vec_to_js(&d.name_translations).into(),
    );
    set(
        &o,
        "descriptionTranslations",
        translations_vec_to_js(&d.description_translations).into(),
    );
    if let Some(i) = d.icon {
        set(&o, "icon", i.into());
    }
    if let Some(c) = d.color_hint {
        set(&o, "colorHint", c.into());
    }
    set(&o, "isManager", d.is_manager.into());
    set(
        &o,
        "defaultVisibilityScope",
        d.default_visibility_scope.into(),
    );
    set(&o, "isActive", d.is_active.into());
    set(&o, "createdAt", d.created_at.into());
    set(&o, "updatedAt", d.updated_at.into());
    if let Some(by) = d.created_by {
        set(&o, "createdBy", by.into());
    }
    o
}

fn user_info_to_js(u: &tentaflow_protocol::UserInfo) -> js_sys::Object {
    let o = js_sys::Object::new();
    set(&o, "id", u.id.clone().into());
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
    for gid in &u.group_ids {
        gs.push(&gid.clone().into());
    }
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
    set(&o, "userId", s.user_id.clone().into());
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
            set(
                obj,
                "deployment",
                deployment_summary_to_js(resp.deployment).into(),
            );
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
            set(obj, "tsMs", c.ts_ms.clone().into());
        }
        DP::StreamEnd(e) => {
            set(obj, "variant", "DeploymentStreamEnd".into());
            set(obj, "deployId", e.deploy_id.into());
            set(obj, "finalStatus", e.final_status.into());
            set(obj, "imageTag", e.image_tag.into());
            set(obj, "containerName", e.container_name.into());
            set(obj, "errorMessage", e.error_message.into());
            set(obj, "durationMs", e.duration_ms.clone().into());
        }
        DP::ReqRedeploy(req) => {
            set(obj, "variant", "ServiceRedeployRequest".into());
            set(obj, "serviceId", (req.service_id as f64).into());
        }
        DP::ResRedeploy(resp) => {
            set(obj, "variant", "ServiceRedeployResponse".into());
            set(obj, "status", resp.status.into());
            set(obj, "deployId", resp.deploy_id.into());
            set(obj, "engineId", resp.engine_id.into());
            set(obj, "deployMethod", resp.deploy_method.into());
            set(obj, "nodeId", resp.node_id.into());
            set(obj, "message", resp.message.into());
        }
    }
}

fn meeting_session_to_js(s: tentaflow_protocol::MeetingSessionDescriptor) -> js_sys::Object {
    let o = js_sys::Object::new();
    set(&o, "sessionId", s.session_id.clone().into());
    set(&o, "meetingKey", s.meeting_key.into());
    set(&o, "meetingUrl", s.meeting_url.into());
    set(&o, "title", s.title.into());
    set(&o, "status", s.status.into());
    set(&o, "startedAt", s.started_at.into());
    set(&o, "lastActivityAt", s.last_activity_at.into());
    set(&o, "endedAt", s.ended_at.into());
    set(&o, "platform", s.platform.into());
    set(&o, "entryCount", s.entry_count.clone().into());
    set(&o, "quicPort", s.quic_port.into());
    set(&o, "vncPort", s.vnc_port.into());
    set(&o, "novncPort", s.novnc_port.into());
    set(&o, "botEndpointId", s.bot_endpoint_id.into());
    set(&o, "containerName", s.container_name.into());
    set(&o, "ownerUserId", s.owner_user_id.clone().into());
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
            v.clone().into()
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
    set(&o, "id", e.id.clone().into());
    set(&o, "sessionId", e.session_id.clone().into());
    set(&o, "timestampMs", e.timestamp_ms.clone().into());
    set(&o, "speaker", e.speaker.into());
    set(&o, "profileId", e.profile_id.clone().into());
    set(&o, "confidence", e.confidence.clone().into());
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
            set(obj, "sessionId", r.session_id.clone().into());
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
            set(
                obj,
                "bytes",
                js_sys::Uint8Array::from(c.bytes.as_slice()).into(),
            );
        }
        VP::ReqSend(r) => {
            set(obj, "variant", "VncTunnelSendRequest".into());
            set(obj, "tunnelId", r.tunnel_id.into());
            set(
                obj,
                "bytes",
                js_sys::Uint8Array::from(r.bytes.as_slice()).into(),
            );
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
        MP::ReqActionItemStatusUpdate(_) => set(
            obj,
            "variant",
            "MeetingActionItemStatusUpdateRequest".into(),
        ),
        MP::ResActionItemStatusUpdate(r) => {
            set(
                obj,
                "variant",
                "MeetingActionItemStatusUpdateResponse".into(),
            );
            set(obj, "success", r.success.into());
        }
        MP::ReqTranscriptExport(_) => set(obj, "variant", "MeetingTranscriptExportRequest".into()),
        MP::ResTranscriptExport(r) => {
            set(obj, "variant", "MeetingTranscriptExportResponse".into());
            set(obj, "content", r.content.into());
        }
        MP::ReqWakeWord(req) => {
            set(obj, "variant", "MeetingWakeWordRequest".into());
            set(obj, "op", wake_word_op_to_js(req.op).into());
        }
        MP::ResWakeWord(r) => {
            set(obj, "variant", "MeetingWakeWordResponse".into());
            let arr = js_sys::Array::new();
            for w in r.words {
                arr.push(&wake_word_to_js(w).into());
            }
            set(obj, "words", arr.into());
        }
    }
}

fn wake_word_to_js(w: tentaflow_protocol::WakeWord) -> js_sys::Object {
    let o = js_sys::Object::new();
    set(&o, "id", w.id.clone().into());
    set(&o, "word", w.word.into());
    set(&o, "enabled", w.enabled.into());
    set(&o, "createdAt", w.created_at.into());
    o
}

fn wake_word_op_to_js(op: tentaflow_protocol::WakeWordOp) -> js_sys::Object {
    use tentaflow_protocol::WakeWordOp as Op;
    let o = js_sys::Object::new();
    match op {
        Op::List => {
            set(&o, "kind", "List".into());
        }
        Op::Create { word } => {
            set(&o, "kind", "Create".into());
            set(&o, "word", word.into());
        }
        Op::Toggle { id, enabled } => {
            set(&o, "kind", "Toggle".into());
            set(&o, "id", id.clone().into());
            set(&o, "enabled", enabled.into());
        }
        Op::Delete { id } => {
            set(&o, "kind", "Delete".into());
            set(&o, "id", id.clone().into());
        }
    }
    o
}

fn meeting_summary_to_js(s: tentaflow_protocol::MeetingSummaryItem) -> js_sys::Object {
    let o = js_sys::Object::new();
    set(&o, "id", s.id.clone().into());
    set(&o, "createdAt", s.created_at.into());
    set(&o, "decisionsText", s.decisions_text.into());
    set(&o, "summaryText", s.summary_text.into());
    set(&o, "model", s.model.into());
    o
}

fn meeting_action_item_to_js(a: tentaflow_protocol::MeetingActionItemItem) -> js_sys::Object {
    let o = js_sys::Object::new();
    set(&o, "id", a.id.clone().into());
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
fn meeting_event_payload_to_js(obj: &js_sys::Object, p: tentaflow_protocol::MeetingEventPayload) {
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
                set(&data, "speakerConfidence", c.clone().into());
            }
            set(&data, "text", text.into());
            if let Some(l) = language {
                set(&data, "language", l.into());
            }
            set(&data, "resolvedSttModel", resolved_stt_model.into());
            set(&data, "latencyMs", latency_ms.clone().into());
        }
        EP::RosterSnapshot { entries } => {
            set(obj, "type", "RosterSnapshot".into());
            let arr = js_sys::Array::new();
            for entry in entries {
                let eo = js_sys::Object::new();
                set(&eo, "speakerId", entry.speaker_id.into());
                if let Some(n) = entry.speaker_name {
                    set(&eo, "speakerName", n.into());
                }
                set(&eo, "status", entry.status.into());
                if let Some(s) = entry.last_spoken_ago_sec {
                    set(&eo, "lastSpokenAgoSec", s.clone().into());
                }
                arr.push(&eo.into());
            }
            set(&data, "entries", arr.into());
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
                set(&data, "streamingLatencyMs", v.clone().into());
            }
            if let Some(v) = enrolled_speakers {
                set(&data, "enrolledSpeakers", v.clone().into());
            }
            if let Some(v) = total_participants {
                set(&data, "totalParticipants", v.clone().into());
            }
        }
        EP::LifecycleUpdate { stage, details } => {
            set(obj, "type", "LifecycleUpdate".into());
            set(&data, "stage", stage.into());
            if let Some(d) = details {
                set(&data, "details", d.into());
            }
        }
        // VideoFrame: surowe JPEG idzie do GUI tylko gdy jest subscriber
        // wymagający podglądu (np. debug overlay). Standardowy live widok
        // korzysta z `ParticipantAttributes` bo te są lekkie. JPEG eksponujemy
        // jako Uint8Array żeby JS mogło zrobić `URL.createObjectURL` bez kopii.
        EP::VideoFrame {
            participant_id,
            name,
            ts_ms,
            jpeg,
        } => {
            set(obj, "type", "VideoFrame".into());
            set(&data, "participantId", participant_id.into());
            if let Some(n) = name {
                set(&data, "name", n.into());
            }
            set(&data, "tsMs", ts_ms.clone().into());
            let arr = js_sys::Uint8Array::new_with_length(jpeg.len() as u32);
            arr.copy_from(&jpeg);
            set(&data, "jpeg", arr.into());
        }
        EP::ParticipantAttributes {
            participant_id,
            name,
            ts_ms,
            emotion,
            emotion_confidence,
            age,
            gender_male_prob,
        } => {
            set(obj, "type", "ParticipantAttributes".into());
            set(&data, "participantId", participant_id.into());
            if let Some(n) = name {
                set(&data, "name", n.into());
            }
            set(&data, "tsMs", ts_ms.clone().into());
            if let Some(e) = emotion {
                set(&data, "emotion", e.into());
            }
            if let Some(c) = emotion_confidence {
                set(&data, "emotionConfidence", c.clone().into());
            }
            if let Some(a) = age {
                set(&data, "age", a.clone().into());
            }
            if let Some(g) = gender_male_prob {
                set(&data, "genderMaleProb", g.clone().into());
            }
        }
    }
    set(obj, "data", data.into());
}

fn flow_node_template_to_js(
    t: tentaflow_protocol::message_body::FlowNodeTemplate,
) -> js_sys::Object {
    let obj = js_sys::Object::new();
    // Emitujemy rownoczesnie camelCase (nowy kod) i snake_case (istniejaca paleta).
    set(&obj, "id", t.id.clone().into());
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
    let input_port_types = js_sys::Array::new();
    for ty in &t.input_port_types {
        input_port_types.push(&JsValue::from_str(ty));
    }
    set(&obj, "inputPortTypes", input_port_types.clone().into());
    set(&obj, "input_port_types", input_port_types.into());
    let output_port_types = js_sys::Array::new();
    for ty in &t.output_port_types {
        output_port_types.push(&JsValue::from_str(ty));
    }
    set(&obj, "outputPortTypes", output_port_types.clone().into());
    set(&obj, "output_port_types", output_port_types.into());
    set(&obj, "paramsSchema", JsValue::from_str(&t.params_schema));
    set(&obj, "params_schema", JsValue::from_str(&t.params_schema));
    obj
}

fn flow_version_summary_to_js(
    v: tentaflow_protocol::message_body::FlowVersionSummary,
) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "id", v.id.into());
    set(&obj, "flowId", v.flow_id.clone().into());
    set(&obj, "flow_id", v.flow_id.into());
    set(&obj, "versionNum", v.version_num.clone().into());
    set(&obj, "version_num", v.version_num.clone().into());
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

fn flow_version_full_to_js(v: tentaflow_protocol::message_body::FlowVersionFull) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "id", v.id.into());
    set(&obj, "flowId", v.flow_id.clone().into());
    set(&obj, "flow_id", v.flow_id.into());
    set(&obj, "versionNum", v.version_num.clone().into());
    set(&obj, "version_num", v.version_num.clone().into());
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
    set(&obj, "id", a.id.clone().into());
    set(&obj, "alias", a.alias.into());
    set(&obj, "targetModel", a.target_model.clone().into());
    set(&obj, "target_model", a.target_model.into());
    set(&obj, "isActive", a.is_active.into());
    set(&obj, "is_active", a.is_active.into());
    if let Some(f) = a.fallback_targets {
        set(&obj, "fallbackTargets", f.clone().into());
        set(&obj, "fallback_targets", f.into());
    }
    if let Some(s) = a.strategy {
        set(&obj, "strategy", s.into());
    }
    obj
}

fn mesh_node_info_to_js(n: tentaflow_protocol::MeshNodeInfo) -> js_sys::Object {
    let obj = js_sys::Object::new();
    // Emitujemy zarowno camelCase (dla nowego kodu) jak i snake_case aliasy
    // (dla istniejacego kodu mesh.js / mesh-detail.js ktory czyta REST-shape).
    set(&obj, "nodeId", n.node_id.clone().into());
    set(&obj, "node_id", n.node_id.into());
    set(&obj, "hostname", n.hostname.into());
    if let Some(ref ip) = n.ip {
        set(&obj, "ip", ip.clone().into());
    }
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
        set(&obj, "cpuUsagePercent", v.clone().into());
        set(&obj, "cpu_usage_percent", v.clone().into());
        set(&obj, "cpu_usage", v.clone().into());
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
        set(&obj, "gpuLoadPercent", v.clone().into());
        set(&obj, "gpu_load_percent", v.clone().into());
    }
    if let Some(connection) = &n.connection {
        let connection_obj = js_sys::Object::new();
        let state_str = match connection.state {
            tentaflow_protocol::MeshConnState::Disconnected => "disconnected",
            tentaflow_protocol::MeshConnState::Connecting => "connecting",
            tentaflow_protocol::MeshConnState::Connected => "connected",
            tentaflow_protocol::MeshConnState::Degraded => "degraded",
            tentaflow_protocol::MeshConnState::Reconnecting => "reconnecting",
            tentaflow_protocol::MeshConnState::Offline => "offline",
        };
        set(&connection_obj, "state", state_str.into());
        set(
            &connection_obj,
            "sinceMs",
            connection.since_ms.clone().into(),
        );
        set(
            &connection_obj,
            "since_ms",
            connection.since_ms.clone().into(),
        );
        set(
            &connection_obj,
            "lastAppHeartbeatMs",
            connection.last_app_heartbeat_ms.clone().into(),
        );
        set(
            &connection_obj,
            "last_app_heartbeat_ms",
            connection.last_app_heartbeat_ms.clone().into(),
        );
        set(
            &connection_obj,
            "transport",
            connection.transport.clone().into(),
        );
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
        // Aggregated `path` view for GUI helpers — kind = "direct"|"relay" with
        // the matching addr/url fields. Picks the selected path; falls back to
        // the first path when nothing is marked selected.
        let path_view = js_sys::Object::new();
        let chosen = connection
            .paths
            .iter()
            .find(|p| p.selected)
            .or_else(|| connection.paths.first());
        if let Some(p) = chosen {
            let kind = if p.transport == "relay" {
                "relay"
            } else {
                "direct"
            };
            set(&path_view, "kind", kind.into());
            if kind == "relay" {
                if let Some(url) = &connection.relay_url {
                    set(&path_view, "url", url.clone().into());
                } else {
                    set(&path_view, "url", p.address.clone().into());
                }
            } else {
                set(&path_view, "addr", p.address.clone().into());
            }
            set(&connection_obj, "path", path_view.into());
        } else if connection.transport == "p2p" || connection.transport == "relay" {
            // No paths list — synth from top-level transport/address.
            let kind = if connection.transport == "relay" {
                "relay"
            } else {
                "direct"
            };
            set(&path_view, "kind", kind.into());
            if kind == "relay" {
                if let Some(url) = &connection.relay_url {
                    set(&path_view, "url", url.clone().into());
                }
            } else if let Some(addr) = &connection.address {
                set(&path_view, "addr", addr.clone().into());
            }
            set(&connection_obj, "path", path_view.into());
        }
        let paths = js_sys::Array::new();
        for path in &connection.paths {
            let path_obj = js_sys::Object::new();
            set(&path_obj, "transport", path.transport.clone().into());
            set(&path_obj, "address", path.address.clone().into());
            set(&path_obj, "selected", path.selected.into());
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
            set(&item, "utilizationPercent", v.clone().into());
            set(&item, "usage_percent", v.clone().into());
        }
        if let Some(v) = g.temperature_c {
            set(&item, "temperatureC", v.clone().into());
            set(&item, "temperature_c", v.clone().into());
        }
        if let Some(v) = g.power_draw_w {
            set(&item, "powerDrawW", v.clone().into());
            set(&item, "power_draw_w", v.clone().into());
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
        if let Some(v) = m.kind {
            set(&item, "kind", v.into());
        }
        if let Some(v) = m.backend {
            set(&item, "backend", v.into());
        }
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
            set(&item, "cpuPercent", v.clone().into());
            set(&item, "cpu_percent", v.clone().into());
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
    set(&obj, "nsys_available", n.nsys_available.into());
    set(&obj, "nsysAvailable", n.nsys_available.into());
    set(&obj, "nsys_version", n.nsys_version.clone().into());
    set(&obj, "nsysVersion", n.nsys_version.into());
    let collectors_arr = js_sys::Array::new();
    for cid in &n.profiling_collectors_available {
        collectors_arr.push(&js_sys::JsString::from(cid.as_str()).into());
    }
    set(
        &obj,
        "profiling_collectors_available",
        collectors_arr.clone().into(),
    );
    set(&obj, "profilingCollectorsAvailable", collectors_arr.into());
    obj
}

fn cluster_member_to_js(m: tentaflow_protocol::ClusterMember) -> js_sys::Object {
    let item = js_sys::Object::new();
    set(&item, "nodeId", m.node_id.into());
    set(&item, "hostname", m.hostname.into());
    set(&item, "status", m.status.into());
    if let Some(t) = m.interface_type {
        set(&item, "interfaceType", t.into());
    }
    if let Some(s) = m.interface_speed_mbps {
        set(&item, "interfaceSpeedMbps", s.into());
    }
    set(&item, "joinedAt", m.joined_at.into());
    if let Some(d) = m.rdma_devices {
        set(&item, "rdmaDevices", d.into());
    }
    if let Some(ip) = m.rdma_ip {
        set(&item, "rdmaIp", ip.into());
    }
    if let Some(s) = m.rdma_socket_ifname {
        set(&item, "rdmaSocketIfname", s.into());
    }
    item
}

fn cluster_info_to_js(c: tentaflow_protocol::ClusterInfo) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set(&obj, "id", c.id.into());
    set(&obj, "name", c.name.into());
    if let Some(d) = c.description {
        set(&obj, "description", d.into());
    }
    set(&obj, "strategy", c.strategy.into());
    set(&obj, "status", c.status.into());
    set(&obj, "membersCount", c.members_count.into());
    set(&obj, "membersOnline", c.members_online.into());
    set(&obj, "createdAt", c.created_at.clone().into());
    set(&obj, "updatedAt", c.updated_at.clone().into());
    set(&obj, "failoverEnabled", c.failover_enabled.into());
    if let Some(t) = c.failover_target {
        set(&obj, "failoverTarget", t.into());
    }
    set(
        &obj,
        "healthCheckIntervalMs",
        c.health_check_interval_ms.into(),
    );
    set(&obj, "timeoutMs", c.timeout_ms.into());
    let members = js_sys::Array::new();
    for m in c.members {
        members.push(&cluster_member_to_js(m).into());
    }
    set(&obj, "members", members.into());
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
    set(&obj, "subjectId", r.subject_id.clone().into());
    set(&obj, "subject_id", r.subject_id.clone().into());
    set(&obj, "permissionId", r.permission_id.clone().into());
    set(&obj, "permission_id", r.permission_id.into());
    set(&obj, "grantMode", r.grant_mode.clone().into());
    set(&obj, "grant_mode", r.grant_mode.into());
    set(&obj, "updatedAtEpoch", r.updated_at_epoch.clone().into());
    set(&obj, "updated_at_epoch", r.updated_at_epoch.clone().into());
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
    set(&obj, "updatedAtEpoch", d.updated_at_epoch.clone().into());
    set(&obj, "updated_at_epoch", d.updated_at_epoch.clone().into());
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
    set(&obj, "updatedAtEpoch", c.updated_at_epoch.clone().into());
    set(&obj, "updated_at_epoch", c.updated_at_epoch.clone().into());
    set(&obj, "oauthMode", c.oauth_mode.clone().into());
    set(&obj, "oauth_mode", c.oauth_mode.into());
    set(
        &obj,
        "linkedAccountsCount",
        c.linked_accounts_count.clone().into(),
    );
    set(
        &obj,
        "linked_accounts_count",
        c.linked_accounts_count.clone().into(),
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
    set(&obj, "id", a.id.clone().into());
    if let Some(uid) = a.user_id {
        set(&obj, "userId", uid.clone().into());
        set(&obj, "user_id", uid.clone().into());
    }
    set(&obj, "addonId", a.addon_id.clone().into());
    set(&obj, "addon_id", a.addon_id.into());
    set(&obj, "providerId", a.provider_id.clone().into());
    set(&obj, "provider_id", a.provider_id.into());
    set(
        &obj,
        "externalAccountId",
        a.external_account_id.clone().into(),
    );
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
        set(&obj, "expiresAtEpoch", v.clone().into());
        set(&obj, "expires_at_epoch", v.clone().into());
    }
    set(&obj, "createdAtEpoch", a.created_at_epoch.clone().into());
    set(&obj, "created_at_epoch", a.created_at_epoch.clone().into());
    if let Some(v) = a.last_used_at_epoch {
        set(&obj, "lastUsedAtEpoch", v.clone().into());
        set(&obj, "last_used_at_epoch", v.clone().into());
    }
    set(&obj, "revoked", a.revoked.into());
    obj
}

/// Konwertuje `MyOAuthEntry` (wiersz widoku "Moje polaczone konta").
fn my_oauth_entry_to_js(e: tentaflow_protocol::message_body::MyOAuthEntry) -> js_sys::Object {
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
    set(
        &obj,
        "providerDisplayName",
        e.provider_display_name.clone().into(),
    );
    set(
        &obj,
        "provider_display_name",
        e.provider_display_name.into(),
    );
    set(&obj, "status", e.status.into());
    if let Some(aid) = e.account_id {
        set(&obj, "accountId", aid.clone().into());
        set(&obj, "account_id", aid.clone().into());
    } else {
        set(&obj, "accountId", JsValue::NULL);
        set(&obj, "account_id", JsValue::NULL);
    }
    set(&obj, "accountEmail", e.account_email.clone().into());
    set(&obj, "account_email", e.account_email.into());
    set(
        &obj,
        "accountDisplayName",
        e.account_display_name.clone().into(),
    );
    set(&obj, "account_display_name", e.account_display_name.into());
    let scopes = js_sys::Array::new();
    for s in e.scopes {
        scopes.push(&JsValue::from_str(&s));
    }
    set(&obj, "scopes", scopes.into());
    set(
        &obj,
        "connectedAtEpoch",
        e.connected_at_epoch.clone().into(),
    );
    set(
        &obj,
        "connected_at_epoch",
        e.connected_at_epoch.clone().into(),
    );
    set(
        &obj,
        "lastUsedAtEpoch",
        e.last_used_at_epoch.clone().into(),
    );
    set(
        &obj,
        "last_used_at_epoch",
        e.last_used_at_epoch.clone().into(),
    );
    set(&obj, "expiresAtEpoch", e.expires_at_epoch.clone().into());
    set(&obj, "expires_at_epoch", e.expires_at_epoch.clone().into());
    obj
}

fn baseline_phase_name(tag: BaselineAdoptPhaseTag) -> &'static str {
    match tag {
        BaselineAdoptPhaseTag::None => "None",
        BaselineAdoptPhaseTag::Elected => "Elected",
        BaselineAdoptPhaseTag::Receiving => "Receiving",
        BaselineAdoptPhaseTag::Importing => "Importing",
        BaselineAdoptPhaseTag::Imported => "Imported",
        BaselineAdoptPhaseTag::Completed => "Completed",
    }
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
        ProtocolErrorCode::Conflict => "Conflict",
        ProtocolErrorCode::NotAvailable => "NotAvailable",
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

/// MessageBody::DeployVllmRecommendRequest. Plynnie przyjmuje JSON
/// (pelne struct DeployVllmRecommendRequest serializowane przez GUI).
#[wasm_bindgen(js_name = encodeDeployVllmRecommendRequest)]
pub fn encode_deploy_vllm_recommend_request(payload_json: String) -> Result<Vec<u8>, JsError> {
    let payload: DeployVllmRecommendRequest = serde_json::from_str(&payload_json)
        .map_err(|e| JsError::new(&format!("payload parse: {e}")))?;
    encode_body_inner(&MessageBody::DeployVllmRecommendRequestBody(payload))
        .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeSuggestServicePortRequest)]
pub fn encode_suggest_service_port_request(payload_json: String) -> Result<Vec<u8>, JsError> {
    let payload: SuggestServicePortRequest = serde_json::from_str(&payload_json)
        .map_err(|e| JsError::new(&format!("payload parse: {e}")))?;
    encode_body_inner(&MessageBody::SuggestServicePortRequestBody(payload))
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
        let body = encode_body_inner(&MessageBody::ModelListRequest).unwrap();
        let frame = encode_envelope_direct_inner(1, 1, 0xF001, body).unwrap();
        assert!(tentaflow_protocol::cbor::decode::<Envelope>(&frame).is_ok());
        assert!(tentaflow_protocol::cbor::decode::<Envelope>(&[]).is_err());
        assert!(tentaflow_protocol::cbor::decode::<Envelope>(&[0u8; 8]).is_err());
        assert!(tentaflow_protocol::cbor::decode::<Envelope>(&frame[..frame.len() / 2]).is_err());
    }

    #[test]
    fn body_encode_decode_round_trip_native() {
        let body = MessageBody::MetaHeartbeat {
            sent_at_epoch: 1_700_000_000,
        };
        let bytes = encode_body_inner(&body).unwrap();
        let decoded = tentaflow_protocol::cbor::decode::<MessageBody>(&bytes).unwrap();
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
pub fn encode_iam_get_user(user_id: String) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqGetUser { user_id })
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
    let group_ids: Vec<String> = group_ids_csv
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    encode_iam(IamPayload::ReqCreateUser {
        username,
        password,
        display_name,
        email,
        role,
        group_ids,
    })
}

#[wasm_bindgen(js_name = encodeIamUpdateUserRequest)]
pub fn encode_iam_update_user(
    user_id: String,
    display_name: String,
    email: String,
    is_active: bool,
    role: String,
) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqUpdateUser {
        user_id,
        display_name,
        email,
        is_active,
        role,
    })
}

#[wasm_bindgen(js_name = encodeIamDeleteUserRequest)]
pub fn encode_iam_delete_user(user_id: String) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqDeleteUser { user_id })
}

#[wasm_bindgen(js_name = encodeIamSetUserGroupsRequest)]
pub fn encode_iam_set_user_groups(
    user_id: String,
    group_ids_csv: String,
) -> Result<Vec<u8>, JsError> {
    let group_ids: Vec<String> = group_ids_csv
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    encode_iam(IamPayload::ReqSetUserGroups { user_id, group_ids })
}

#[wasm_bindgen(js_name = encodeIamResetUserPasswordRequest)]
pub fn encode_iam_reset_password(
    user_id: String,
    new_password: String,
) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqResetUserPassword {
        user_id,
        new_password,
    })
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
pub fn encode_iam_update_group(
    group_id: String,
    name: String,
    description: String,
) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqUpdateGroup {
        group_id,
        name,
        description,
    })
}

#[wasm_bindgen(js_name = encodeIamDeleteGroupRequest)]
pub fn encode_iam_delete_group(group_id: String) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqDeleteGroup { group_id })
}

#[wasm_bindgen(js_name = encodeIamGroupMembersRequest)]
pub fn encode_iam_group_members(group_id: String) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqGroupMembers { group_id })
}

#[wasm_bindgen(js_name = encodeIamSetPermissionRequest)]
pub fn encode_iam_set_permission(
    resource_type: String,
    resource_id: String,
    subject_type: String,
    subject_id: String,
    access_level: String,
) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqSetPermission {
        resource_type,
        resource_id,
        subject_type,
        subject_id,
        access_level,
    })
}

#[wasm_bindgen(js_name = encodeIamClearPermissionRequest)]
pub fn encode_iam_clear_permission(
    resource_type: String,
    resource_id: String,
    subject_type: String,
    subject_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqClearPermission {
        resource_type,
        resource_id,
        subject_type,
        subject_id,
    })
}

#[wasm_bindgen(js_name = encodeIamListPermsForResourceRequest)]
pub fn encode_iam_list_perms_resource(
    resource_type: String,
    resource_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqListPermsForResource {
        resource_type,
        resource_id,
    })
}

#[wasm_bindgen(js_name = encodeIamListPermsForSubjectRequest)]
pub fn encode_iam_list_perms_subject(
    subject_type: String,
    subject_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_iam(IamPayload::ReqListPermsForSubject {
        subject_type,
        subject_id,
    })
}

// =============================================================================
// AddonUi encoders (Apps menu + UI v2). Schema v14.
// =============================================================================

use tentaflow_protocol::AddonUiPayload;

fn encode_addon_ui(payload: AddonUiPayload) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::AddonUiBody(payload)).map_err(|e| JsError::new(&e))
}

/// MessageBody::AddonUiBody(ReqApplicationsList) — lista aplikacji widocznych
/// w glownym menu launcher. Frontend buduje liste ikon w app menu.
#[wasm_bindgen(js_name = encodeAddonApplicationsListRequest)]
pub fn encode_addon_applications_list_request() -> Result<Vec<u8>, JsError> {
    encode_addon_ui(AddonUiPayload::ReqApplicationsList)
}

// =============================================================================
// Network settings encoders (interfejsy hosta + konfiguracja bind/filter).
// Wrapuja NetworkPayload w MessageBody::NetworkBody i serializuja CBOR.
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
    set(&obj, "mtu", iface.mtu.clone().into());
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
    set(
        &obj,
        "excludedInterfaces",
        string_vec_to_js(cfg.excluded_interfaces.clone()).into(),
    );
    set(
        &obj,
        "excluded_interfaces",
        string_vec_to_js(cfg.excluded_interfaces.clone()).into(),
    );
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
    excluded_interfaces: Vec<String>,
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
        excluded_interfaces,
    }))
}

// =============================================================================
// Multi-source profiling (V2) — encode/decode dla 7 par r/r.
// Pakowane w `MessageBody::ProfilingBody(ProfilingPayload)`.
// =============================================================================

fn gpu_vendor_to_js(v: &tentaflow_protocol::GpuVendor) -> JsValue {
    use tentaflow_protocol::GpuVendor as V;
    match v {
        V::Nvidia => "nvidia".into(),
        V::Amd => "amd".into(),
        V::Intel => "intel".into(),
        V::Apple => "apple".into(),
    }
}

fn gpu_vendor_from_str(s: &str) -> Result<tentaflow_protocol::GpuVendor, JsError> {
    use tentaflow_protocol::GpuVendor as V;
    match s.to_ascii_lowercase().as_str() {
        "nvidia" => Ok(V::Nvidia),
        "amd" => Ok(V::Amd),
        "intel" => Ok(V::Intel),
        "apple" => Ok(V::Apple),
        other => Err(JsError::new(&format!(
            "gpu vendor: nieznany '{other}' (oczekiwany nvidia|amd|intel|apple)"
        ))),
    }
}

fn gpu_targets_to_js(t: &tentaflow_protocol::GpuTargets) -> JsValue {
    use tentaflow_protocol::GpuTargets as G;
    match t {
        G::None => "none".into(),
        G::All => "all".into(),
        G::Indices(idx) => {
            let arr = js_sys::Array::new();
            for i in idx {
                arr.push(&i.clone().into());
            }
            let o = js_sys::Object::new();
            set(&o, "indices", arr.into());
            o.into()
        }
        G::ByVendor(v) => {
            let o = js_sys::Object::new();
            set(&o, "byVendor", gpu_vendor_to_js(v));
            o.into()
        }
    }
}

fn gpu_targets_from_js(value: &JsValue) -> Result<tentaflow_protocol::GpuTargets, JsError> {
    use tentaflow_protocol::GpuTargets as G;
    if let Some(s) = value.as_string() {
        return match s.to_ascii_lowercase().as_str() {
            "none" => Ok(G::None),
            "all" => Ok(G::All),
            other => Err(JsError::new(&format!(
                "gpuTargets: nieznany string '{other}' (oczekiwany none|all albo obiekt)"
            ))),
        };
    }
    if value.is_object() {
        let obj: &js_sys::Object = value.unchecked_ref();
        let indices_js = js_sys::Reflect::get(obj, &"indices".into())
            .map_err(|_| JsError::new("gpuTargets: blad odczytu pola"))?;
        if !indices_js.is_undefined() && !indices_js.is_null() {
            if !indices_js.is_array() {
                return Err(JsError::new("gpuTargets.indices: oczekiwana tablica liczb"));
            }
            let arr = js_sys::Array::from(&indices_js);
            let mut out = Vec::with_capacity(arr.length() as usize);
            for i in 0..arr.length() {
                let v = arr.get(i);
                let n = v
                    .as_f64()
                    .ok_or_else(|| JsError::new("gpuTargets.indices: element musi byc liczba"))?;
                if !(0.0..=u32::MAX as f64).contains(&n) || n.fract() != 0.0 {
                    return Err(JsError::new(
                        "gpuTargets.indices: liczba poza zakresem u32 lub niecalkowita",
                    ));
                }
                out.push(n as u32);
            }
            return Ok(G::Indices(out));
        }
        let by_vendor = js_sys::Reflect::get(obj, &"byVendor".into())
            .map_err(|_| JsError::new("gpuTargets: blad odczytu byVendor"))?;
        if !by_vendor.is_undefined() && !by_vendor.is_null() {
            let s = by_vendor
                .as_string()
                .ok_or_else(|| JsError::new("gpuTargets.byVendor: oczekiwany string"))?;
            return Ok(G::ByVendor(gpu_vendor_from_str(&s)?));
        }
        return Err(JsError::new(
            "gpuTargets: obiekt musi miec pole 'indices' albo 'byVendor'",
        ));
    }
    Err(JsError::new(
        "gpuTargets: oczekiwany 'none'|'all' albo obiekt {indices}|{byVendor}",
    ))
}

fn profile_target_to_js(t: &tentaflow_protocol::ProfileTarget) -> JsValue {
    use tentaflow_protocol::ProfileTarget as T;
    match t {
        T::SystemWide => "system_wide".into(),
        T::OwnProcess => "own_process".into(),
        T::Pid(pid) => {
            let o = js_sys::Object::new();
            set(&o, "pid", pid.clone().into());
            o.into()
        }
    }
}

fn profile_target_from_js(value: &JsValue) -> Result<tentaflow_protocol::ProfileTarget, JsError> {
    use tentaflow_protocol::ProfileTarget as T;
    if let Some(s) = value.as_string() {
        return match s.as_str() {
            "system_wide" | "SystemWide" => Ok(T::SystemWide),
            "own_process" | "OwnProcess" => Ok(T::OwnProcess),
            other => Err(JsError::new(&format!(
                "target: nieznany string '{other}' (oczekiwany system_wide|own_process albo {{pid}})"
            ))),
        };
    }
    if value.is_object() {
        let obj: &js_sys::Object = value.unchecked_ref();
        let pid_js = js_sys::Reflect::get(obj, &"pid".into())
            .map_err(|_| JsError::new("target: blad odczytu 'pid'"))?;
        let pid = pid_js
            .as_f64()
            .ok_or_else(|| JsError::new("target.pid: oczekiwana liczba"))?;
        if !(0.0..=u32::MAX as f64).contains(&pid) || pid.fract() != 0.0 {
            return Err(JsError::new("target.pid: liczba poza zakresem u32"));
        }
        return Ok(T::Pid(pid as u32));
    }
    Err(JsError::new(
        "target: oczekiwany string albo obiekt {pid: u32}",
    ))
}

fn profile_source_flags_from_js(
    value: &JsValue,
) -> Result<tentaflow_protocol::ProfileSourceFlags, JsError> {
    let n = value
        .as_f64()
        .ok_or_else(|| JsError::new("sources: oczekiwana liczba (bitmask u32)"))?;
    if !(0.0..=u32::MAX as f64).contains(&n) || n.fract() != 0.0 {
        return Err(JsError::new("sources: liczba poza zakresem u32"));
    }
    Ok(tentaflow_protocol::ProfileSourceFlags(n as u32))
}

fn profile_scope_from_js(value: &JsValue) -> Result<tentaflow_protocol::ProfileScope, JsError> {
    if !value.is_object() {
        return Err(JsError::new("scope: oczekiwany obiekt"));
    }
    let obj: &js_sys::Object = value.unchecked_ref();

    let sources_js = js_sys::Reflect::get(obj, &"sources".into())
        .map_err(|_| JsError::new("scope: brak pola 'sources'"))?;
    let sources = profile_source_flags_from_js(&sources_js)?;

    let gpu_js = js_sys::Reflect::get(obj, &"gpuTargets".into())
        .map_err(|_| JsError::new("scope: brak pola 'gpuTargets'"))?;
    let gpu_targets = gpu_targets_from_js(&gpu_js)?;

    let hz_js = js_sys::Reflect::get(obj, &"cpuSamplingHz".into())
        .map_err(|_| JsError::new("scope: brak pola 'cpuSamplingHz'"))?;
    let hz = hz_js
        .as_f64()
        .ok_or_else(|| JsError::new("scope.cpuSamplingHz: oczekiwana liczba"))?;
    if !(0.0..=u32::MAX as f64).contains(&hz) || hz.fract() != 0.0 {
        return Err(JsError::new(
            "scope.cpuSamplingHz: niecalkowita lub poza u32",
        ));
    }
    let cpu_sampling_hz = hz as u32;

    let target_js = js_sys::Reflect::get(obj, &"target".into())
        .map_err(|_| JsError::new("scope: brak pola 'target'"))?;
    let target = profile_target_from_js(&target_js)?;

    let dur_js = js_sys::Reflect::get(obj, &"durationSeconds".into())
        .map_err(|_| JsError::new("scope: brak pola 'durationSeconds'"))?;
    let dur = dur_js
        .as_f64()
        .ok_or_else(|| JsError::new("scope.durationSeconds: oczekiwana liczba"))?;
    if !(0.0..=u32::MAX as f64).contains(&dur) || dur.fract() != 0.0 {
        return Err(JsError::new(
            "scope.durationSeconds: niecalkowita lub poza u32",
        ));
    }
    let duration_seconds = dur as u32;

    let label_js = js_sys::Reflect::get(obj, &"label".into())
        .map_err(|_| JsError::new("scope: brak pola 'label'"))?;
    let label = label_js
        .as_string()
        .ok_or_else(|| JsError::new("scope.label: oczekiwany string"))?;

    let scope = tentaflow_protocol::ProfileScope {
        sources,
        gpu_targets,
        cpu_sampling_hz,
        target,
        duration_seconds,
        label,
    };
    scope
        .validate()
        .map_err(|e| JsError::new(&format!("invalid scope: {e}")))?;
    Ok(scope)
}

fn profile_scope_to_js(s: &tentaflow_protocol::ProfileScope) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "sources", s.sources.0.clone().into());
    set(&o, "gpuTargets", gpu_targets_to_js(&s.gpu_targets));
    set(&o, "cpuSamplingHz", s.cpu_sampling_hz.clone().into());
    set(&o, "target", profile_target_to_js(&s.target));
    set(&o, "durationSeconds", s.duration_seconds.clone().into());
    set(&o, "label", s.label.clone().into());
    o.into()
}

fn event_category_to_js(c: tentaflow_protocol::EventCategory) -> JsValue {
    use tentaflow_protocol::EventCategory as E;
    match c {
        E::CpuSample => "cpu_sample",
        E::CpuCounter => "cpu_counter",
        E::CpuUtil => "cpu_util",
        E::RamSample => "ram_sample",
        E::RamBandwidth => "ram_bandwidth",
        E::DiskIoBurst => "disk_io_burst",
        E::GpuKernel => "gpu_kernel",
        E::GpuApiCall => "gpu_api_call",
        E::GpuUtilSample => "gpu_util_sample",
        E::GpuMemSample => "gpu_mem_sample",
        E::GpuMemTransfer => "gpu_mem_transfer",
        E::PowerSample => "power_sample",
        E::NvtxRange => "nvtx_range",
        E::NetworkSample => "network_sample",
        E::ProcessRssSample => "process_rss_sample",
        E::ProcessIoSample => "process_io_sample",
        E::Custom => "custom",
    }
    .into()
}

fn power_domain_to_js(d: &tentaflow_protocol::PowerDomain) -> JsValue {
    use tentaflow_protocol::PowerDomain as P;
    match d {
        P::CpuPkg => "cpu_pkg".into(),
        P::CpuCore => "cpu_core".into(),
        P::Dram => "dram".into(),
        P::Ane => "ane".into(),
        P::Soc => "soc".into(),
        P::Other => "other".into(),
        P::Gpu(idx) => {
            let o = js_sys::Object::new();
            set(&o, "kind", "gpu".into());
            set(&o, "index", idx.clone().into());
            o.into()
        }
    }
}

fn counter_kind_to_js(k: &tentaflow_protocol::CounterKind) -> JsValue {
    use tentaflow_protocol::CounterKind as C;
    match k {
        C::Ipc => "ipc".into(),
        C::CacheMissL1 => "cache_miss_l1".into(),
        C::CacheMissL2 => "cache_miss_l2".into(),
        C::CacheMissL3 => "cache_miss_l3".into(),
        C::BranchMiss => "branch_miss".into(),
        C::ContextSwitches => "context_switches".into(),
        C::PageFaults => "page_faults".into(),
        C::TlbMiss => "tlb_miss".into(),
        C::Custom(name) => {
            let o = js_sys::Object::new();
            set(&o, "kind", "custom".into());
            set(&o, "name", name.clone().into());
            o.into()
        }
    }
}

fn transfer_kind_to_js(k: tentaflow_protocol::TransferKind) -> JsValue {
    use tentaflow_protocol::TransferKind as T;
    match k {
        T::H2D => "h2d",
        T::D2H => "d2h",
        T::D2D => "d2d",
        T::UnifiedAccess => "unified_access",
    }
    .into()
}

fn collector_status_to_js(s: &tentaflow_protocol::CollectorStatus) -> JsValue {
    use tentaflow_protocol::CollectorStatus as S;
    let o = js_sys::Object::new();
    match s {
        S::Used => set(&o, "kind", "used".into()),
        S::SkippedUnavailable(reason) => {
            set(&o, "kind", "skipped_unavailable".into());
            set(&o, "reason", reason.clone().into());
        }
        S::SkippedRequiresElevation => set(&o, "kind", "skipped_requires_elevation".into()),
        S::Failed(reason) => {
            set(&o, "kind", "failed".into());
            set(&o, "reason", reason.clone().into());
        }
    }
    o.into()
}

fn collector_run_info_to_js(c: &tentaflow_protocol::CollectorRunInfo) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "id", c.id.clone().into());
    set(&o, "status", collector_status_to_js(&c.status));
    set(&o, "samplesCollected", c.samples_collected.clone().into());
    set(&o, "rawSizeBytes", c.raw_size_bytes.clone().into());
    set(
        &o,
        "primaryCategory",
        event_category_to_js(c.primary_category),
    );
    set(&o, "durationNs", c.duration_ns.clone().into());
    o.into()
}

fn frame_to_js(f: &tentaflow_protocol::Frame) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "symbol", f.symbol.clone().into());
    set(&o, "module", f.module.clone().into());
    set(
        &o,
        "file",
        match &f.file {
            Some(s) => s.clone().into(),
            None => JsValue::NULL,
        },
    );
    set(
        &o,
        "line",
        match f.line {
            Some(n) => n.clone().into(),
            None => JsValue::NULL,
        },
    );
    o.into()
}

fn u32_array_to_js(arr: &[u32]) -> JsValue {
    let out = js_sys::Array::new();
    for v in arr {
        out.push(&v.clone().into());
    }
    out.into()
}

fn event_payload_to_js(p: &tentaflow_protocol::EventPayload) -> JsValue {
    use tentaflow_protocol::EventPayload as P;
    let o = js_sys::Object::new();
    match p {
        P::CpuSample { tid, cpu, stack_id } => {
            set(&o, "kind", "cpu_sample".into());
            set(&o, "tid", tid.clone().into());
            set(&o, "cpu", cpu.clone().into());
            set(&o, "stackId", stack_id.clone().into());
        }
        P::CpuCounter { kind, value } => {
            set(&o, "kind", "cpu_counter".into());
            set(&o, "counter", counter_kind_to_js(kind));
            set(&o, "value", (*value).into());
        }
        P::CpuUtil {
            core,
            util_pct,
            freq_mhz,
        } => {
            set(&o, "kind", "cpu_util".into());
            set(&o, "core", core.clone().into());
            set(&o, "utilPct", util_pct.clone().into());
            set(&o, "freqMhz", freq_mhz.clone().into());
        }
        P::RamSample {
            used_bytes,
            available_bytes,
            page_faults_per_s,
        } => {
            set(&o, "kind", "ram_sample".into());
            set(&o, "usedBytes", used_bytes.clone().into());
            set(&o, "availableBytes", available_bytes.clone().into());
            set(&o, "pageFaultsPerS", page_faults_per_s.clone().into());
        }
        P::RamBandwidth {
            read_bps,
            write_bps,
        } => {
            set(&o, "kind", "ram_bandwidth".into());
            set(&o, "readBps", read_bps.clone().into());
            set(&o, "writeBps", write_bps.clone().into());
        }
        P::DiskIoBurst {
            device_name_id,
            read_bps,
            write_bps,
            iops_r,
            iops_w,
            await_ms_p99,
        } => {
            set(&o, "kind", "disk_io_burst".into());
            // Device label is interned in `ProfileReportV2.names`; the GUI
            // resolves the string via `names[deviceNameId]`.
            set(&o, "deviceNameId", device_name_id.clone().into());
            set(&o, "readBps", read_bps.clone().into());
            set(&o, "writeBps", write_bps.clone().into());
            set(&o, "iopsR", iops_r.clone().into());
            set(&o, "iopsW", iops_w.clone().into());
            set(&o, "awaitMsP99", await_ms_p99.clone().into());
        }
        P::GpuKernel {
            device_id,
            name_id,
            grid,
            block,
            shared_mem_bytes,
        } => {
            set(&o, "kind", "gpu_kernel".into());
            set(&o, "deviceId", device_id.clone().into());
            set(&o, "nameId", name_id.clone().into());
            set(&o, "grid", u32_array_to_js(grid));
            set(&o, "block", u32_array_to_js(block));
            set(&o, "sharedMemBytes", shared_mem_bytes.clone().into());
        }
        P::GpuApiCall {
            device_id,
            name_id,
            return_code,
        } => {
            set(&o, "kind", "gpu_api_call".into());
            set(&o, "deviceId", device_id.clone().into());
            set(&o, "nameId", name_id.clone().into());
            set(&o, "returnCode", return_code.clone().into());
        }
        P::GpuUtilSample {
            device_id,
            compute_pct,
            mem_pct,
            mem_used_bytes,
            temp_c,
        } => {
            set(&o, "kind", "gpu_util_sample".into());
            set(&o, "deviceId", device_id.clone().into());
            set(&o, "computePct", compute_pct.clone().into());
            set(&o, "memPct", mem_pct.clone().into());
            set(&o, "memUsedBytes", mem_used_bytes.clone().into());
            set(&o, "tempC", temp_c.clone().into());
        }
        P::GpuMemSample {
            device_id,
            allocated_bytes,
            free_bytes,
        } => {
            set(&o, "kind", "gpu_mem_sample".into());
            set(&o, "deviceId", device_id.clone().into());
            set(&o, "allocatedBytes", allocated_bytes.clone().into());
            set(&o, "freeBytes", free_bytes.clone().into());
        }
        P::GpuMemTransfer {
            device_id,
            kind,
            bytes,
        } => {
            set(&o, "kind", "gpu_mem_transfer".into());
            set(&o, "deviceId", device_id.clone().into());
            set(&o, "transferKind", transfer_kind_to_js(*kind));
            set(&o, "bytes", bytes.clone().into());
        }
        P::PowerSample { domain, watts } => {
            set(&o, "kind", "power_sample".into());
            set(&o, "domain", power_domain_to_js(domain));
            set(&o, "watts", watts.clone().into());
        }
        P::NvtxRange {
            device_id,
            name_id,
            color,
        } => {
            set(&o, "kind", "nvtx_range".into());
            set(&o, "deviceId", device_id.clone().into());
            set(&o, "nameId", name_id.clone().into());
            set(&o, "color", color.clone().into());
        }
        P::NetworkSample {
            iface_name_id,
            rx_bps,
            tx_bps,
            rx_pps,
            tx_pps,
        } => {
            set(&o, "kind", "network_sample".into());
            // Interface label is interned in `ProfileReportV2.names`.
            set(&o, "ifaceNameId", iface_name_id.clone().into());
            set(&o, "rxBps", rx_bps.clone().into());
            set(&o, "txBps", tx_bps.clone().into());
            set(&o, "rxPps", rx_pps.clone().into());
            set(&o, "txPps", tx_pps.clone().into());
        }
        P::Custom { name_id, value } => {
            set(&o, "kind", "custom".into());
            set(&o, "nameId", name_id.clone().into());
            set(&o, "value", (*value).into());
        }
        P::ProcessRssSample {
            pid,
            comm_name_id,
            rss_bytes,
            vsz_bytes,
        } => {
            set(&o, "kind", "process_rss_sample".into());
            set(&o, "pid", pid.clone().into());
            set(&o, "commNameId", comm_name_id.clone().into());
            set(&o, "rssBytes", rss_bytes.clone().into());
            set(&o, "vszBytes", vsz_bytes.clone().into());
        }
        P::ProcessIoSample {
            pid,
            comm_name_id,
            read_bytes,
            write_bytes,
        } => {
            set(&o, "kind", "process_io_sample".into());
            set(&o, "pid", pid.clone().into());
            set(&o, "commNameId", comm_name_id.clone().into());
            set(&o, "readBytes", read_bytes.clone().into());
            set(&o, "writeBytes", write_bytes.clone().into());
        }
    }
    o.into()
}

fn timeline_event_to_js(e: &tentaflow_protocol::TimelineEvent) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "sourceIdx", e.source_idx.clone().into());
    set(&o, "tStartNs", e.t_start_ns.clone().into());
    set(&o, "tEndNs", e.t_end_ns.clone().into());
    set(&o, "category", event_category_to_js(e.category));
    set(&o, "laneHint", e.lane_hint.clone().into());
    set(&o, "payload", event_payload_to_js(&e.payload));
    o.into()
}

fn clock_samples_to_js(c: &tentaflow_protocol::ClockSamples) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "collectorId", c.collector_id.clone().into());
    let pairs = js_sys::Array::new();
    for (a, b) in &c.pairs {
        let p = js_sys::Array::new();
        p.push(&a.clone().into());
        p.push(&b.clone().into());
        pairs.push(&p.into());
    }
    set(&o, "pairs", pairs.into());
    o.into()
}

fn drift_report_to_js(d: &tentaflow_protocol::DriftReport) -> JsValue {
    let o = js_sys::Object::new();
    let arr = js_sys::Array::new();
    for s in &d.per_collector {
        arr.push(&clock_samples_to_js(s));
    }
    set(&o, "perCollector", arr.into());
    set(
        &o,
        "maxObservedDriftNs",
        d.max_observed_drift_ns.clone().into(),
    );
    set(&o, "exceededTolerance", d.exceeded_tolerance.into());
    set(&o, "toleranceNs", d.tolerance_ns.clone().into());
    o.into()
}

fn profile_report_v2_to_js(r: &tentaflow_protocol::ProfileReportV2) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "schemaVersion", r.schema_version.clone().into());
    set(&o, "sessionId", r.session_id.clone().into());
    set(&o, "nodeId", r.node_id.clone().into());
    set(&o, "scope", profile_scope_to_js(&r.scope));
    set(&o, "t0MonotonicNs", r.t0_monotonic_ns.clone().into());
    set(
        &o,
        "t0WallclockUnixNs",
        r.t0_wallclock_unix_ns.clone().into(),
    );
    set(&o, "durationNs", r.duration_ns.clone().into());

    let collectors = js_sys::Array::new();
    for c in &r.collectors {
        collectors.push(&collector_run_info_to_js(c));
    }
    set(&o, "collectors", collectors.into());

    let events = js_sys::Array::new();
    for e in &r.events {
        events.push(&timeline_event_to_js(e));
    }
    set(&o, "events", events.into());

    let frames = js_sys::Array::new();
    for f in &r.frames {
        frames.push(&frame_to_js(f));
    }
    set(&o, "frames", frames.into());

    let stacks = js_sys::Array::new();
    for stack in &r.stacks {
        stacks.push(&u32_array_to_js(stack));
    }
    set(&o, "stacks", stacks.into());

    let names = js_sys::Array::new();
    for n in &r.names {
        names.push(&JsValue::from_str(n));
    }
    set(&o, "names", names.into());

    set(&o, "driftReport", drift_report_to_js(&r.drift_report));

    let warnings = js_sys::Array::new();
    for w in &r.warnings {
        warnings.push(&JsValue::from_str(w));
    }
    set(&o, "warnings", warnings.into());

    o.into()
}

fn profiling_skipped_collector_to_js(s: &tentaflow_protocol::ProfilingSkippedCollector) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "id", s.id.clone().into());
    set(&o, "reason", s.reason.clone().into());
    o.into()
}

fn profiling_session_entry_to_js(e: &tentaflow_protocol::ProfilingSessionEntry) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "sessionId", e.session_id.clone().into());
    set(&o, "label", e.label.clone().into());
    set(&o, "startedAt", e.started_at.clone().into());
    set(&o, "durationNs", e.duration_ns.clone().into());
    set(&o, "kind", e.kind.clone().into());
    let cols = js_sys::Array::new();
    for c in &e.collectors_used {
        cols.push(&JsValue::from_str(c));
    }
    set(&o, "collectorsUsed", cols.into());
    set(&o, "sizeBytes", e.size_bytes.clone().into());
    o.into()
}

fn profiling_active_session_info_to_js(
    info: &tentaflow_protocol::ProfilingActiveSessionInfo,
) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "sessionId", info.session_id.clone().into());
    set(&o, "nodeId", info.node_id.clone().into());
    set(&o, "label", info.label.clone().into());
    set(
        &o,
        "startedAtUnixNs",
        info.started_at_unix_ns.clone().into(),
    );
    set(
        &o,
        "plannedDurationNs",
        info.planned_duration_ns.clone().into(),
    );
    set(&o, "elapsedNs", info.elapsed_ns.clone().into());
    let running = js_sys::Array::new();
    for c in &info.collectors_running {
        running.push(&JsValue::from_str(c));
    }
    set(&o, "collectorsRunning", running.into());
    let skipped = js_sys::Array::new();
    for s in &info.collectors_skipped {
        skipped.push(&profiling_skipped_collector_to_js(s));
    }
    set(&o, "collectorsSkipped", skipped.into());
    o.into()
}

/// Wypelnia `obj` polami pojedynczego wariantu `ProfilingPayload`.
fn profiling_payload_fill_obj(
    obj: &js_sys::Object,
    payload: &tentaflow_protocol::ProfilingPayload,
) {
    use tentaflow_protocol::ProfilingPayload as P;
    match payload {
        P::StartRequest(r) => {
            set(obj, "variant", "ProfilingStartRequest".into());
            set(obj, "nodeId", r.node_id.clone().into());
            set(obj, "scope", profile_scope_to_js(&r.scope));
            set(obj, "label", r.label.clone().into());
            // Hasla nie eksponujemy w decode (bezpieczenstwo); JS dostaje tylko fakt obecnosci.
            set(
                obj,
                "hasElevationPassword",
                (!r.elevation_password.is_empty()).into(),
            );
        }
        P::StartResponse(r) => {
            set(obj, "variant", "ProfilingStartResponse".into());
            set(obj, "sessionId", r.session_id.clone().into());
            set(obj, "startedAtUnixNs", r.started_at_unix_ns.clone().into());
            let started = js_sys::Array::new();
            for c in &r.collectors_started {
                started.push(&JsValue::from_str(c));
            }
            set(obj, "collectorsStarted", started.into());
            let skipped = js_sys::Array::new();
            for s in &r.collectors_skipped {
                skipped.push(&profiling_skipped_collector_to_js(s));
            }
            set(obj, "collectorsSkipped", skipped.into());
        }
        P::StopRequest(r) => {
            set(obj, "variant", "ProfilingStopRequest".into());
            set(obj, "nodeId", r.node_id.clone().into());
            set(obj, "sessionId", r.session_id.clone().into());
        }
        P::StopResponse(r) => {
            set(obj, "variant", "ProfilingStopResponse".into());
            set(obj, "sessionId", r.session_id.clone().into());
            set(obj, "report", profile_report_v2_to_js(&r.report));
        }
        P::SessionsRequest(r) => {
            set(obj, "variant", "ProfilingSessionsRequest".into());
            set(obj, "nodeId", r.node_id.clone().into());
        }
        P::SessionsResponse(r) => {
            set(obj, "variant", "ProfilingSessionsResponse".into());
            set(obj, "nodeId", r.node_id.clone().into());
            let entries = js_sys::Array::new();
            for e in &r.entries {
                entries.push(&profiling_session_entry_to_js(e));
            }
            set(obj, "entries", entries.into());
        }
        P::ReportRequest(r) => {
            set(obj, "variant", "ProfilingReportRequest".into());
            set(obj, "nodeId", r.node_id.clone().into());
            set(obj, "sessionId", r.session_id.clone().into());
        }
        P::ReportResponse(r) => {
            set(obj, "variant", "ProfilingReportResponse".into());
            set(obj, "report", profile_report_v2_to_js(&r.report));
        }
        P::DeleteRequest(r) => {
            set(obj, "variant", "ProfilingDeleteRequest".into());
            set(obj, "nodeId", r.node_id.clone().into());
            set(obj, "sessionId", r.session_id.clone().into());
        }
        P::DeleteResponse(r) => {
            set(obj, "variant", "ProfilingDeleteResponse".into());
            set(obj, "sessionId", r.session_id.clone().into());
            set(obj, "deleted", r.deleted.into());
        }
        P::DownloadRequest(r) => {
            set(obj, "variant", "ProfilingDownloadRequest".into());
            set(obj, "nodeId", r.node_id.clone().into());
            set(obj, "sessionId", r.session_id.clone().into());
        }
        P::DownloadResponse(r) => {
            set(obj, "variant", "ProfilingDownloadResponse".into());
            set(obj, "sessionId", r.session_id.clone().into());
            set(obj, "filename", r.filename.clone().into());
            set(
                obj,
                "tarballBytes",
                js_sys::Uint8Array::from(r.tarball_bytes.as_slice()).into(),
            );
        }
        P::ActiveInfoRequest(r) => {
            set(obj, "variant", "ProfilingActiveInfoRequest".into());
            set(obj, "nodeId", r.node_id.clone().into());
        }
        P::ActiveInfoResponse(r) => {
            set(obj, "variant", "ProfilingActiveInfoResponse".into());
            match &r.info {
                Some(info) => set(obj, "info", profiling_active_session_info_to_js(info)),
                None => set(obj, "info", JsValue::NULL),
            }
        }
        P::ValidateSudoRequest(r) => {
            set(obj, "variant", "ProfilingValidateSudoRequest".into());
            set(obj, "nodeId", r.node_id.clone().into());
        }
        P::ValidateSudoResponse(r) => {
            set(obj, "variant", "ProfilingValidateSudoResponse".into());
            set(obj, "ok", r.ok.into());
            set(obj, "message", r.message.clone().into());
            set(obj, "reason", r.reason.clone().into());
        }
        P::CollectorsStatusRequest(r) => {
            set(obj, "variant", "ProfilingCollectorsStatusRequest".into());
            set(obj, "nodeId", r.node_id.clone().into());
        }
        P::CollectorsStatusResponse(r) => {
            set(obj, "variant", "ProfilingCollectorsStatusResponse".into());
            let arr = js_sys::Array::new();
            for c in &r.collectors {
                arr.push(&profiling_collector_status_to_js(c));
            }
            set(obj, "collectors", arr.into());
            set(obj, "ageSeconds", r.age_seconds.clone().into());
        }
    }
}

fn profiling_collector_status_to_js(c: &tentaflow_protocol::ProfilingCollectorStatus) -> JsValue {
    let o = js_sys::Object::new();
    set(&o, "id", c.id.clone().into());
    set(&o, "name", c.name.clone().into());
    set(&o, "available", c.available.into());
    set(
        &o,
        "version",
        c.version
            .clone()
            .map(JsValue::from)
            .unwrap_or(JsValue::NULL),
    );
    set(
        &o,
        "path",
        c.path.clone().map(JsValue::from).unwrap_or(JsValue::NULL),
    );
    set(&o, "needsSudo", c.needs_sudo.into());
    set(
        &o,
        "note",
        c.note.clone().map(JsValue::from).unwrap_or(JsValue::NULL),
    );
    o.into()
}

fn encode_profiling(p: tentaflow_protocol::ProfilingPayload) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ProfilingBody(p)).map_err(|e| JsError::new(&e))
}

/// MessageBody::ProfilingBody(ProfilingPayload::StartRequest(..)).
#[wasm_bindgen(js_name = encodeProfilingStartRequest)]
pub fn encode_profiling_start_request(
    node_id: String,
    scope: JsValue,
    label: String,
    elevation_password: Option<String>,
) -> Result<Vec<u8>, JsError> {
    let scope = profile_scope_from_js(&scope)?;
    encode_profiling(tentaflow_protocol::ProfilingPayload::StartRequest(
        tentaflow_protocol::ProfilingStartRequest {
            node_id,
            scope,
            label,
            elevation_password: elevation_password.unwrap_or_default(),
        },
    ))
}

/// MessageBody::ProfilingBody(ProfilingPayload::StopRequest(..)).
#[wasm_bindgen(js_name = encodeProfilingStopRequest)]
pub fn encode_profiling_stop_request(
    node_id: String,
    session_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_profiling(tentaflow_protocol::ProfilingPayload::StopRequest(
        tentaflow_protocol::ProfilingStopRequest {
            node_id,
            session_id,
        },
    ))
}

/// MessageBody::ProfilingBody(ProfilingPayload::SessionsRequest(..)).
#[wasm_bindgen(js_name = encodeProfilingSessionsRequest)]
pub fn encode_profiling_sessions_request(node_id: String) -> Result<Vec<u8>, JsError> {
    encode_profiling(tentaflow_protocol::ProfilingPayload::SessionsRequest(
        tentaflow_protocol::ProfilingSessionsRequest { node_id },
    ))
}

/// MessageBody::ProfilingBody(ProfilingPayload::ReportRequest(..)).
#[wasm_bindgen(js_name = encodeProfilingReportRequest)]
pub fn encode_profiling_report_request(
    node_id: String,
    session_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_profiling(tentaflow_protocol::ProfilingPayload::ReportRequest(
        tentaflow_protocol::ProfilingReportRequest {
            node_id,
            session_id,
        },
    ))
}

/// MessageBody::ProfilingBody(ProfilingPayload::DeleteRequest(..)).
#[wasm_bindgen(js_name = encodeProfilingDeleteRequest)]
pub fn encode_profiling_delete_request(
    node_id: String,
    session_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_profiling(tentaflow_protocol::ProfilingPayload::DeleteRequest(
        tentaflow_protocol::ProfilingDeleteRequest {
            node_id,
            session_id,
        },
    ))
}

/// MessageBody::ProfilingBody(ProfilingPayload::DownloadRequest(..)).
#[wasm_bindgen(js_name = encodeProfilingDownloadRequest)]
pub fn encode_profiling_download_request(
    node_id: String,
    session_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_profiling(tentaflow_protocol::ProfilingPayload::DownloadRequest(
        tentaflow_protocol::ProfilingDownloadRequest {
            node_id,
            session_id,
        },
    ))
}

/// MessageBody::ProfilingBody(ProfilingPayload::ActiveInfoRequest(..)).
#[wasm_bindgen(js_name = encodeProfilingActiveInfoRequest)]
pub fn encode_profiling_active_info_request(node_id: String) -> Result<Vec<u8>, JsError> {
    encode_profiling(tentaflow_protocol::ProfilingPayload::ActiveInfoRequest(
        tentaflow_protocol::ProfilingActiveInfoRequest { node_id },
    ))
}

/// MessageBody::ProfilingBody(ProfilingPayload::ValidateSudoRequest(..)).
#[wasm_bindgen(js_name = encodeProfilingValidateSudoRequest)]
pub fn encode_profiling_validate_sudo_request(
    node_id: String,
    password: String,
) -> Result<Vec<u8>, JsError> {
    encode_profiling(tentaflow_protocol::ProfilingPayload::ValidateSudoRequest(
        tentaflow_protocol::ProfilingValidateSudoRequest { node_id, password },
    ))
}

/// MessageBody::ProfilingBody(ProfilingPayload::CollectorsStatusRequest(..)).
#[wasm_bindgen(js_name = encodeProfilingCollectorsStatusRequest)]
pub fn encode_profiling_collectors_status_request(node_id: String) -> Result<Vec<u8>, JsError> {
    encode_profiling(
        tentaflow_protocol::ProfilingPayload::CollectorsStatusRequest(
            tentaflow_protocol::ProfilingCollectorsStatusRequest { node_id },
        ),
    )
}

// =============================================================================
// Camera admin (F2 P7.a-bis) — wizard dashboard RPCs packed into
// `MessageBody::CameraAdminBody(CameraAdminPayload)`.
// =============================================================================

/// MessageBody::CameraAdminBody(DiscoverRequest) — kick off ONVIF WS-Discovery
/// against the local network; the response carries the discovered devices.
#[wasm_bindgen(js_name = encodeCameraDiscoverRequest)]
pub fn encode_camera_discover_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::CameraAdminBody(
        tentaflow_protocol::CameraAdminPayload::DiscoverRequest(
            tentaflow_protocol::CameraDiscoverRequest {},
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::CameraAdminBody(AddOnvifRequest) — bind a discovered ONVIF
/// device as a managed camera session. Credentials travel over the TLS
/// admin transport and are AES-GCM-sealed server-side before persistence.
#[wasm_bindgen(js_name = encodeCameraAddOnvifRequest)]
pub fn encode_camera_add_onvif_request(
    display_name: String,
    device_service_url: String,
    username: String,
    password: String,
    profile_token: Option<String>,
    target_fps: Option<u32>,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::CameraAdminBody(
        tentaflow_protocol::CameraAdminPayload::AddOnvifRequest(
            tentaflow_protocol::CameraAddOnvifRequest {
                display_name,
                device_service_url,
                username,
                password,
                profile_token,
                target_fps,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::CameraAdminBody(FrameUrlRequest) — live-preview tile URL
/// for `<tf-live-camera-tile>`. The handler gates on `camera.read`,
/// enforces UUID v4 camera_id validation, a per-user rate limit, and a
/// 5..=300 s dispatch TTL band before minting against the global frame
/// signed-URL issuer.
#[wasm_bindgen(js_name = encodeCameraFrameUrlRequest)]
pub fn encode_camera_frame_url_request(
    camera_id: String,
    ttl_secs: u32,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::CameraAdminBody(
        tentaflow_protocol::CameraAdminPayload::FrameUrlRequest(
            tentaflow_protocol::CameraFrameUrlRequest {
                camera_id,
                ttl_secs,
            },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::CameraAdminBody(DetectionsSubscribeRequest) — open a per-camera
/// detection overlay stream. The handler validates the `cam_<uuid v4>` id,
/// gates on `camera.read` + org isolation, and replies with a long-lived
/// stream of `CameraDetectionsFrame` chunks until cancel/disconnect.
#[wasm_bindgen(js_name = encodeCameraDetectionsSubscribeRequest)]
pub fn encode_camera_detections_subscribe_request(
    camera_id: String,
) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::CameraAdminBody(
        tentaflow_protocol::CameraAdminPayload::DetectionsSubscribeRequest(
            tentaflow_protocol::CameraDetectionsSubscribeRequest { camera_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

// =============================================================================
// Legal admin (F2 P8.d-bis) — RODO/GDPR document RPCs packed into
// `MessageBody::LegalAdminBody(LegalAdminPayload)`.
// =============================================================================

/// MessageBody::LegalAdminBody(ListRequest) — fetch the legal documents
/// catalogue. `include_revoked = false` matches the default dashboard view.
#[wasm_bindgen(js_name = encodeLegalDocumentsListRequest)]
pub fn encode_legal_documents_list_request(include_revoked: bool) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::LegalAdminBody(
        tentaflow_protocol::LegalAdminPayload::ListRequest(
            tentaflow_protocol::LegalDocumentsListRequest { include_revoked },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::LegalAdminBody(GenerateRequest) — render and persist a new
/// RODO/GDPR PDF. `variant` must be one of `short` | `standard` | `full`
/// (server-side validation via `RodoVariant::from_str`).
#[wasm_bindgen(js_name = encodeLegalDocumentGenerateRequest)]
pub fn encode_legal_document_generate_request(variant: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::LegalAdminBody(
        tentaflow_protocol::LegalAdminPayload::GenerateRequest(
            tentaflow_protocol::LegalDocumentGenerateRequest { variant },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::LegalAdminBody(RevokeRequest) — soft-delete a previously
/// generated legal document. The PDF stays on disk; the row gets a
/// `revoked_at` stamp and is excluded from default list views.
#[wasm_bindgen(js_name = encodeLegalDocumentRevokeRequest)]
pub fn encode_legal_document_revoke_request(doc_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::LegalAdminBody(
        tentaflow_protocol::LegalAdminPayload::RevokeRequest(
            tentaflow_protocol::LegalDocumentRevokeRequest { doc_id },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

// =============================================================================
// Compliance Core — `MessageBody::ComplianceAdminBody(ComplianceAdminPayload)`.
// =============================================================================

#[wasm_bindgen(js_name = encodeComplianceDataCategoriesListRequest)]
pub fn encode_compliance_data_categories_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ComplianceAdminBody(
        tentaflow_protocol::ComplianceAdminPayload::ListDataCategoriesRequest,
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeComplianceRetentionPoliciesListRequest)]
pub fn encode_compliance_retention_policies_list_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::ComplianceAdminBody(
        tentaflow_protocol::ComplianceAdminPayload::ListRetentionPoliciesRequest,
    ))
    .map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = encodeComplianceAiEventsListRequest)]
pub fn encode_compliance_ai_events_list_request(
    status: Option<String>,
    user_id: Option<String>,
    addon_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Vec<u8>, JsError> {
    let filter = tentaflow_protocol::ComplianceAiEventListFilter {
        status,
        user_id,
        addon_id,
        limit,
        offset,
    };
    encode_body_inner(&MessageBody::ComplianceAdminBody(
        tentaflow_protocol::ComplianceAdminPayload::ListAiEventsRequest(filter),
    ))
    .map_err(|e| JsError::new(&e))
}

// =============================================================================
// Stream pub/sub (Chunk B) — `MessageBody::StreamBody(StreamPayload)`.
// Client only encodes the two request variants; frame / closed / response
// variants are server-issued and travel through `decode_message_body_inner`.
// =============================================================================

/// MessageBody::StreamBody(SubscribeRequest) — subscribe this connection to a
/// hub-registered stream. The server first answers with a SubscribeResponse
/// (mime + has_init_segment), then pushes a sequence of Frame chunks on the
/// same correlation id, terminating with a single Closed payload. `preview`
/// wybiera wariant podglądu 720p/~1,5 Mbit/s dla strumieni `camera:` (kafelki
/// Live view); `false` = pełna jakość źródła.
#[wasm_bindgen(js_name = encodeStreamSubscribeRequest)]
pub fn encode_stream_subscribe_request(stream_id: String, preview: bool) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::StreamBody(
        tentaflow_protocol::StreamPayload::SubscribeRequest(
            tentaflow_protocol::StreamSubscribeRequest { stream_id, preview },
        ),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::StreamBody(CloseRequest) — release a live subscription early
/// (e.g. UI tile navigates away). Reuses the original correlation id; the
/// server cancels the streaming task and emits a final Closed frame.
#[wasm_bindgen(js_name = encodeStreamCloseRequest)]
pub fn encode_stream_close_request(stream_id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::StreamBody(
        tentaflow_protocol::StreamPayload::CloseRequest(tentaflow_protocol::StreamCloseRequest {
            stream_id,
        }),
    ))
    .map_err(|e| JsError::new(&e))
}

// =============================================================================
// Role catalog (administrowalny katalog rol biznesowych) — `MessageBody::RoleCatalogBody`.
// Payloady są bogate (Vec krotek, Option<Option<String>>), więc przyjmujemy
// JSON string z UI i parsujemy do typów DTO przed enkapsulacją w CBOR.
// =============================================================================

/// MessageBody::RoleCatalogBody(ListRequest) — filter jako JSON object.
/// Wszystkie pola filter opcjonalne; pusty `{}` zwraca pelna liste.
#[wasm_bindgen(js_name = encodeRoleCatalogListRequest)]
pub fn encode_role_catalog_list_request(filter_json: String) -> Result<Vec<u8>, JsError> {
    use serde_json::Value;
    let v: Value = serde_json::from_str(&filter_json)
        .map_err(|e| JsError::new(&format!("invalid filter JSON: {e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| JsError::new("filter must be JSON object"))?;
    let filter = tentaflow_protocol::RoleCatalogListFilter {
        kind: obj.get("kind").and_then(|x| x.as_str()).map(String::from),
        is_active: obj.get("is_active").and_then(|x| x.as_bool()),
        search: obj.get("search").and_then(|x| x.as_str()).map(String::from),
        limit: obj.get("limit").and_then(|x| x.as_u64()).map(|n| n as u32),
        offset: obj.get("offset").and_then(|x| x.as_u64()).map(|n| n as u32),
    };
    encode_body_inner(&MessageBody::RoleCatalogBody(
        tentaflow_protocol::RoleCatalogPayload::ListRequest(filter),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::RoleCatalogBody(GetRequest { id }).
#[wasm_bindgen(js_name = encodeRoleCatalogGetRequest)]
pub fn encode_role_catalog_get_request(id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::RoleCatalogBody(
        tentaflow_protocol::RoleCatalogPayload::GetRequest { id },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::RoleCatalogBody(GetBySlugRequest { slug }).
#[wasm_bindgen(js_name = encodeRoleCatalogGetBySlugRequest)]
pub fn encode_role_catalog_get_by_slug_request(slug: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::RoleCatalogBody(
        tentaflow_protocol::RoleCatalogPayload::GetBySlugRequest { slug },
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::RoleCatalogBody(ListLocalesRequest) — unit variant.
#[wasm_bindgen(js_name = encodeRoleCatalogListLocalesRequest)]
pub fn encode_role_catalog_list_locales_request() -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::RoleCatalogBody(
        tentaflow_protocol::RoleCatalogPayload::ListLocalesRequest,
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::RoleCatalogBody(CreateRequest) — payload jako JSON object
/// odpowiadajacy `RoleCatalogCreateRequest`. Translations sa parami
/// `[code, value]`; brak ikony / color_hint w obiekcie = None.
#[wasm_bindgen(js_name = encodeRoleCatalogCreateRequest)]
pub fn encode_role_catalog_create_request(payload_json: String) -> Result<Vec<u8>, JsError> {
    use serde_json::Value;
    let v: Value = serde_json::from_str(&payload_json)
        .map_err(|e| JsError::new(&format!("invalid create payload JSON: {e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| JsError::new("create payload must be JSON object"))?;

    let slug = obj
        .get("slug")
        .and_then(|x| x.as_str())
        .ok_or_else(|| JsError::new("missing 'slug'"))?
        .to_string();
    let kind = obj
        .get("kind")
        .and_then(|x| x.as_str())
        .ok_or_else(|| JsError::new("missing 'kind'"))?
        .to_string();

    let parse_pairs = |key: &str| -> Result<Vec<(String, String)>, JsError> {
        let arr = obj
            .get(key)
            .and_then(|x| x.as_array())
            .ok_or_else(|| JsError::new(&format!("missing or invalid '{key}'")))?;
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let pair = item
                .as_array()
                .ok_or_else(|| JsError::new(&format!("{key} item must be [code,value]")))?;
            if pair.len() != 2 {
                return Err(JsError::new(&format!("{key} item must be [code,value]")));
            }
            let code = pair[0]
                .as_str()
                .ok_or_else(|| JsError::new(&format!("{key} code must be string")))?
                .to_string();
            let value = pair[1]
                .as_str()
                .ok_or_else(|| JsError::new(&format!("{key} value must be string")))?
                .to_string();
            out.push((code, value));
        }
        Ok(out)
    };
    let name_translations = parse_pairs("name_translations")?;
    let description_translations = parse_pairs("description_translations")?;

    let opt_str = |key: &str| -> Option<String> {
        match obj.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        }
    };
    let icon = opt_str("icon");
    let color_hint = opt_str("color_hint");

    let is_manager = obj
        .get("is_manager")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let default_visibility_scope = obj
        .get("default_visibility_scope")
        .and_then(|x| x.as_str())
        .ok_or_else(|| JsError::new("missing 'default_visibility_scope'"))?
        .to_string();

    let req = tentaflow_protocol::RoleCatalogCreateRequest {
        slug,
        kind,
        name_translations,
        description_translations,
        icon,
        color_hint,
        is_manager,
        default_visibility_scope,
    };
    encode_body_inner(&MessageBody::RoleCatalogBody(
        tentaflow_protocol::RoleCatalogPayload::CreateRequest(req),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::RoleCatalogBody(UpdateRequest) — patch update.
/// `Option<Option<String>>` w JSON: brak pola = nie ruszaj, `null` = wyzeruj,
/// string = ustaw. Vec<(String, String)> jako lista par `[["pl","..."], ...]`.
/// Serde nie potrafi rozroznic "missing" od "null" dla `Option<Option<T>>`,
/// wiec parsujemy ręcznie z `serde_json::Value`.
#[wasm_bindgen(js_name = encodeRoleCatalogUpdateRequest)]
pub fn encode_role_catalog_update_request(payload_json: String) -> Result<Vec<u8>, JsError> {
    use serde_json::Value;
    let v: Value = serde_json::from_str(&payload_json)
        .map_err(|e| JsError::new(&format!("invalid update payload JSON: {e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| JsError::new("update payload must be JSON object"))?;

    let id = obj
        .get("id")
        .and_then(|x| x.as_str())
        .ok_or_else(|| JsError::new("missing 'id' in update payload"))?
        .to_string();

    let kind = obj
        .get("kind")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());

    let parse_pairs = |key: &str| -> Result<Option<Vec<(String, String)>>, JsError> {
        let Some(arr) = obj.get(key) else {
            return Ok(None);
        };
        let Some(arr) = arr.as_array() else {
            return Err(JsError::new(&format!("{key} must be array")));
        };
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let pair = item
                .as_array()
                .ok_or_else(|| JsError::new(&format!("{key} item must be [code,value]")))?;
            if pair.len() != 2 {
                return Err(JsError::new(&format!("{key} item must be [code,value]")));
            }
            let code = pair[0]
                .as_str()
                .ok_or_else(|| JsError::new(&format!("{key} item code must be string")))?
                .to_string();
            let value = pair[1]
                .as_str()
                .ok_or_else(|| JsError::new(&format!("{key} item value must be string")))?
                .to_string();
            out.push((code, value));
        }
        Ok(Some(out))
    };
    let name_translations = parse_pairs("name_translations")?;
    let description_translations = parse_pairs("description_translations")?;

    // Option<Option<String>>: obj.get == None -> None (brak pola = nie ruszaj),
    // obj.get == Some(Null) -> Some(None) (clear), obj.get == Some(String) -> Some(Some).
    let parse_double_opt = |key: &str| -> Result<Option<Option<String>>, JsError> {
        match obj.get(key) {
            None => Ok(None),
            Some(Value::Null) => Ok(Some(None)),
            Some(Value::String(s)) => Ok(Some(Some(s.clone()))),
            Some(_) => Err(JsError::new(&format!(
                "{key} must be string or null when present"
            ))),
        }
    };
    let icon = parse_double_opt("icon")?;
    let color_hint = parse_double_opt("color_hint")?;

    let is_manager = obj.get("is_manager").and_then(|x| x.as_bool());
    let default_visibility_scope = obj
        .get("default_visibility_scope")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());

    let req = tentaflow_protocol::RoleCatalogUpdateRequest {
        id,
        kind,
        name_translations,
        description_translations,
        icon,
        color_hint,
        is_manager,
        default_visibility_scope,
    };
    encode_body_inner(&MessageBody::RoleCatalogBody(
        tentaflow_protocol::RoleCatalogPayload::UpdateRequest(req),
    ))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::RoleCatalogBody(DeactivateRequest { id }).
#[wasm_bindgen(js_name = encodeRoleCatalogDeactivateRequest)]
pub fn encode_role_catalog_deactivate_request(id: String) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::RoleCatalogBody(
        tentaflow_protocol::RoleCatalogPayload::DeactivateRequest { id },
    ))
    .map_err(|e| JsError::new(&e))
}

// =============================================================================
// UI Channel CBOR (Faza 6 Krok 4) — `MessageBody::UiChannelCbor(Vec<u8>)`.
// The CBOR bytes are opaque to the outer MessageBody; the browser JS codec encodes
// the UiPayload as CBOR itself and wraps it in this variant for transport.
// =============================================================================

/// Wraps raw CBOR bytes in `MessageBody::UiChannelCbor` for binary WS transport.
#[wasm_bindgen(js_name = encodeUiChannelCbor)]
pub fn encode_ui_channel_cbor(cbor_bytes: &[u8]) -> Result<Vec<u8>, JsError> {
    encode_body_inner(&MessageBody::UiChannelCbor(cbor_bytes.to_vec()))
        .map_err(|e| JsError::new(&e))
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
    use tentaflow_sdk_spec::UiPayload;
    use tentaflow_sdk_spec::protocol::ui::panel::{PanelOpen, PanelOpenContext, Viewport};

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
    use tentaflow_sdk_spec::UiPayload;
    use tentaflow_sdk_spec::protocol::ui::panel::{CloseReason, PanelClose};

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
    use tentaflow_sdk_spec::UiPayload;
    use tentaflow_sdk_spec::protocol::ui::action::Action;

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
                set(&s_obj, "cache_policy", cache_policy_to_js(&slot.cache_policy)?);
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
    encode_body_inner(&MessageBody::UiChannelCbor(cbor_buf)).map_err(|e| JsError::new(&e))
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
    getrandom::getrandom(&mut buf).unwrap_or_default();
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

/// MessageBody::RobotsBody(ListRequest) — org-scoped robot list.
#[wasm_bindgen(js_name = encodeRobotsListRequest)]
pub fn encode_robots_list_request() -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{RobotsListRequest, RobotsPayload};
    encode_body_inner(&MessageBody::RobotsBody(RobotsPayload::ListRequest(
        RobotsListRequest,
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::RobotsBody(ControlRequest) — route a typed, allowlisted action to
/// the robot's owning node. The `vx`/`vy`/`vyaw` axes apply to "move" only; the
/// `p1..p4` generic params carry parametered poses/levels keyed by `kind` (euler
/// → roll/pitch/yaw; body_height/foot_raise_height → p1=height; speed_level →
/// p1=level; pose → roll/pitch/yaw/height). The owner clamps every numeric param
/// to the documented Go2 range.
#[wasm_bindgen(js_name = encodeRobotControlRequest)]
#[allow(clippy::too_many_arguments)]
pub fn encode_robot_control_request(
    robot_id: String,
    kind: String,
    vx: f64,
    vy: f64,
    vyaw: f64,
    p1: f64,
    p2: f64,
    p3: f64,
    p4: f64,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{RobotActionWire, RobotControlRequest, RobotsPayload};
    encode_body_inner(&MessageBody::RobotsBody(RobotsPayload::ControlRequest(
        RobotControlRequest {
            robot_id,
            action: RobotActionWire { kind, vx, vy, vyaw, p1, p2, p3, p4 },
        },
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::RobotsBody(CameraShareRequest) — expose a robot's camera to
/// TentaVision (local grant) or surface the remote-view note (remote robot).
#[wasm_bindgen(js_name = encodeRobotCameraShareRequest)]
pub fn encode_robot_camera_share_request(
    robot_id: String,
    camera_id: String,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{RobotCameraShareRequest, RobotsPayload};
    encode_body_inner(&MessageBody::RobotsBody(RobotsPayload::CameraShareRequest(
        RobotCameraShareRequest {
            robot_id,
            camera_id,
        },
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::RobotsBody(GeoAnchorSetRequest) — pin the robot's scene origin to a
/// real-world lat/lon/alt + heading (all `Some` = set; all `None` = clear).
#[wasm_bindgen(js_name = encodeRobotGeoAnchorSetRequest)]
pub fn encode_robot_geo_anchor_set_request(
    robot_id: String,
    lat: Option<f64>,
    lon: Option<f64>,
    alt: Option<f64>,
    heading: Option<f64>,
) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{RobotGeoAnchorSetRequest, RobotsPayload};
    encode_body_inner(&MessageBody::RobotsBody(RobotsPayload::GeoAnchorSetRequest(
        RobotGeoAnchorSetRequest { robot_id, lat, lon, alt, heading },
    )))
    .map_err(|e| JsError::new(&e))
}

/// MessageBody::RobotsBody(GeoAnchorGetRequest) — read a robot's geo anchor + live
/// real-world position.
#[wasm_bindgen(js_name = encodeRobotGeoAnchorGetRequest)]
pub fn encode_robot_geo_anchor_get_request(robot_id: String) -> Result<Vec<u8>, JsError> {
    use tentaflow_protocol::{RobotGeoAnchorGetRequest, RobotsPayload};
    encode_body_inner(&MessageBody::RobotsBody(RobotsPayload::GeoAnchorGetRequest(
        RobotGeoAnchorGetRequest { robot_id },
    )))
    .map_err(|e| JsError::new(&e))
}

