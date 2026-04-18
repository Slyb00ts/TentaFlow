// =============================================================================
// Plik: dispatch/handlers.rs
// Opis: Bootstrap handlery MessageBody z #[handler] / #[policy] / #[observed].
//       Po #36 (bulk migration) tu przeniesie sie 75+ kolejnych. Kazda funkcja
//       ma signature:
//         fn name(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError>
//       Kompilator wymusza #[policy] + #[observed] przez compile-gate w #[handler].
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    ApiKeyCreateResponse, ApiKeySummary, AuthLoginResponse, AuthMeResponse, ChatStreamChunk,
    ChatStreamEnd, ClusterUpdateResponse, DashboardSnapshot, MeshPairInitResponse,
    MeshPeerSummary, MessageBody, ModelSummary, NodeSummary, ProtocolError, ProtocolErrorCode,
    SettingEntry,
};

use super::HandlerContext;

// =============================================================================
// NodeListRequest — zwraca liste nodow mesh.
// Policy: UserSession (tylko GUI ma dostep — admin view).
// =============================================================================

#[handler(variant = "NodeListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn node_list_request(
    _req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    // Bootstrap: zwracamy placeholder. Integracja z MeshPeerStore przychodzi w #36.
    Ok(MessageBody::NodeListResponse {
        nodes: vec![NodeSummary {
            node_id: [0u8; 32],
            display_name: "this-node".to_string(),
            status: "online".to_string(),
            role: "leader".to_string(),
            is_self: true,
        }],
    })
}

// =============================================================================
// ModelListRequest — publiczny katalog modeli.
// Policy: Anonymous (read-only, bez auth).
// =============================================================================

#[handler(variant = "ModelListRequest", since = (1, 0))]
#[policy(Anonymous)]
#[observed]
pub fn model_list_request(
    _req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    // Bootstrap: placeholder. Integracja z rejestrem modeli w #36.
    Ok(MessageBody::ModelListResponse {
        models: vec![ModelSummary {
            id: "placeholder".to_string(),
            category: "llm".to_string(),
            engine_id: "none".to_string(),
            availability: "not-installed".to_string(),
        }],
    })
}

// =============================================================================
// MetaHeartbeat — echo keepalive dla RTT measurement.
// Policy: Anonymous (keepalive dostepny na kazdej sesji).
// =============================================================================

#[handler(variant = "MetaHeartbeat", since = (1, 0))]
#[policy(Anonymous)]
#[observed]
pub fn meta_heartbeat(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::MetaHeartbeat { sent_at_epoch } => Ok(MessageBody::MetaHeartbeat {
            sent_at_epoch: *sent_at_epoch,
        }),
        _ => Err(ProtocolError::bad_request(
            "meta_heartbeat expected MetaHeartbeat variant",
        )),
    }
}

// =============================================================================
// MetaCancelStream — anulacja streama (placeholder, bo brak aktywnych streamow).
// Policy: Anonymous (client moze anulowac swoj wlasny stream).
// =============================================================================

#[handler(variant = "MetaCancelStream", since = (1, 0))]
#[policy(Anonymous)]
#[observed]
pub fn meta_cancel_stream(
    _req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    Err(ProtocolError::new(
        ProtocolErrorCode::StreamCancelled,
        "no active stream for this correlation_id",
    ))
}

// =============================================================================
// NodeInfoRequest — szczegoly pojedynczego noda.
// Policy: UserSession (admin dashboard).
// =============================================================================

#[handler(variant = "NodeInfoRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn node_info_request(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::NodeInfoRequest { node_id: _ } => Err(ProtocolError::not_found(
            "node info not implemented yet (bootstrap stub)",
        )),
        _ => Err(ProtocolError::bad_request(
            "node_info_request expected NodeInfoRequest variant",
        )),
    }
}

// =============================================================================
// API Keys — R-LIST + W-CREATE + W-DELETE archetypy.
// Policy: UserSession (wlasciciel API key).
// =============================================================================

#[handler(variant = "ApiKeyListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn api_key_list_request(
    _req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    Ok(MessageBody::ApiKeyListResponse {
        keys: vec![ApiKeySummary {
            key_id: "placeholder".to_string(),
            name: "bootstrap".to_string(),
            created_at_epoch: 0,
            last_used_at_epoch: None,
        }],
    })
}

#[handler(variant = "ApiKeyCreateRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn api_key_create(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::ApiKeyCreateRequestBody(payload) => {
            Ok(MessageBody::ApiKeyCreateResponseBody(ApiKeyCreateResponse {
                key_id: format!("key-{}", payload.name),
                token: "bootstrap-stub-token".to_string(),
            }))
        }
        _ => Err(ProtocolError::bad_request(
            "api_key_create expected ApiKeyCreateRequestBody variant",
        )),
    }
}

#[handler(variant = "ApiKeyRevokeRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn api_key_revoke(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::ApiKeyRevokeRequest { key_id: _ } => {
            Ok(MessageBody::ApiKeyRevokeResponse { deleted: true })
        }
        _ => Err(ProtocolError::bad_request(
            "api_key_revoke expected ApiKeyRevokeRequest variant",
        )),
    }
}

// =============================================================================
// Auth — W-ACTION + R-ONE archetypy.
// Login: Anonymous (login endpoint musi byc dostepny bez sesji).
// Me: UserSession (wymaga zalogowanego uzytkownika).
// =============================================================================

#[handler(variant = "AuthLoginRequest", since = (1, 0))]
#[policy(Anonymous)]
#[observed]
pub fn auth_login(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::AuthLoginRequestBody(_creds) => {
            Ok(MessageBody::AuthLoginResponseBody(AuthLoginResponse {
                jwt: "bootstrap-jwt-token".to_string(),
                user_id: [0u8; 16],
                role: "user".to_string(),
            }))
        }
        _ => Err(ProtocolError::bad_request(
            "auth_login expected AuthLoginRequestBody variant",
        )),
    }
}

#[handler(variant = "AuthMeRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn auth_me(
    _req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    Ok(MessageBody::AuthMeResponseBody(AuthMeResponse {
        user_id: [0u8; 16],
        username: "bootstrap".to_string(),
        role: "user".to_string(),
    }))
}

// =============================================================================
// Chat stream — R-STREAM archetyp (bootstrap: placeholder chunk + end).
// Policy: UserSession (zwykly rate-limited chat, autoryzacja wymagana).
// =============================================================================

#[handler(variant = "ChatStreamRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn chat_stream_request(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::ChatStreamRequestBody(_payload) => {
            // Bootstrap: zwracamy od razu "end" (pusty stream). Prawdziwe streaming
            // dispatch wymaga zmiany signatury HandlerMeta::dispatch_fn na async
            // fn zwracajaca stream — to refactor w #34 + #36 phase 2.
            Ok(MessageBody::ChatStreamEndBody(ChatStreamEnd {
                prompt_tokens: 0,
                completion_tokens: 0,
            }))
        }
        _ => Err(ProtocolError::bad_request(
            "chat_stream_request expected ChatStreamRequestBody variant",
        )),
    }
}

// Odebrane chunki od serwera (od innego noda) nie maja lokalnego handlera —
// sa emitowane do klienta prosto z mpsc. Tymczasowy echo jest niepotrzebny.
#[handler(variant = "ChatStreamChunk", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn chat_stream_chunk(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::ChatStreamChunkBody(chunk) => {
            // Echo dla bootstrap (pozwala unit-test round-trip).
            Ok(MessageBody::ChatStreamChunkBody(ChatStreamChunk {
                delta: chunk.delta.clone(),
            }))
        }
        _ => Err(ProtocolError::bad_request(
            "chat_stream_chunk expected ChatStreamChunkBody variant",
        )),
    }
}

// =============================================================================
// Cluster — W-UPDATE archetyp.
// Policy: UserSession (admin-only ograniczenie dodamy w #36 phase 2 — teraz
// bootstrap na UserSession zeby handler byl w rejestrze).
// =============================================================================

#[handler(variant = "ClusterUpdateRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn cluster_update(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::ClusterUpdateRequestBody(payload) => {
            Ok(MessageBody::ClusterUpdateResponseBody(ClusterUpdateResponse {
                cluster_id: payload.cluster_id.clone(),
                updated_at_epoch: 0,
            }))
        }
        _ => Err(ProtocolError::bad_request(
            "cluster_update expected ClusterUpdateRequestBody variant",
        )),
    }
}

// =============================================================================
// Mesh peers — R-LIST + W-ACTION archetypy.
// Policy: UserSession dla list (dashboard view), UserSession dla pair init
// (pairing wymaga zalogowanego admina — docelowo SessionAuthKind::Admin w phase 2).
// =============================================================================

#[handler(variant = "MeshPeersListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn mesh_peers_list(
    _req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    Ok(MessageBody::MeshPeersListResponse {
        peers: vec![MeshPeerSummary {
            node_id: [0u8; 32],
            display_name: "self".to_string(),
            trust_state: "trusted".to_string(),
            endpoint: None,
            last_seen_epoch: None,
        }],
    })
}

#[handler(variant = "MeshPairInitRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn mesh_pair_init(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::MeshPairInitRequestBody(_) => {
            Ok(MessageBody::MeshPairInitResponseBody(MeshPairInitResponse {
                pair_id: "bootstrap-pair".to_string(),
                expires_at_epoch: 0,
            }))
        }
        _ => Err(ProtocolError::bad_request(
            "mesh_pair_init expected MeshPairInitRequestBody variant",
        )),
    }
}

// =============================================================================
// Settings — R-LIST + W-UPDATE archetypy.
// Policy: UserSession (tylko zalogowany admin/user moze czytac/pisac settings).
// =============================================================================

#[handler(variant = "SettingsListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn settings_list(
    _req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    Ok(MessageBody::SettingsListResponse {
        entries: vec![SettingEntry {
            key: "bootstrap".to_string(),
            value: "placeholder".to_string(),
            is_secret: false,
        }],
    })
}

#[handler(variant = "SettingsUpdateRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn settings_update(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::SettingsUpdateRequestBody(payload) => Ok(MessageBody::SettingsUpdateResponse {
            applied: payload.entries.len() as u32,
        }),
        _ => Err(ProtocolError::bad_request(
            "settings_update expected SettingsUpdateRequestBody variant",
        )),
    }
}

// =============================================================================
// SubscribeResumeRequest — klient resume po disconnect.
// Bootstrap: weryfikujemy token signature, ale buffer replay z recorder
// to phase 2 (#34). Tu zwracamy SubscribeResumeAck { accepted: false } z
// reasonem zeby klient wiedzial ze trzeba subscribe od zera.
// Policy: UserSession (tej samej tier co oryginalny stream).
// =============================================================================

#[handler(variant = "SubscribeResumeRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn subscribe_resume_request(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::SubscribeResumeRequest { resume_token } => {
            // Bootstrap: signature check tylko (HMAC), buffer replay TBD.
            // Real secret pozyskamy z db settings w phase 2; tu stub.
            let _ = resume_token;
            Ok(MessageBody::SubscribeResumeAck {
                accepted: false,
                error: Some("resume not implemented yet — please re-subscribe".to_string()),
            })
        }
        _ => Err(ProtocolError::bad_request(
            "subscribe_resume_request expected SubscribeResumeRequest variant",
        )),
    }
}

// =============================================================================
// Dashboard metrics — R-LIST, subscription candidate (subskrypcja w #36 phase 2).
// Policy: UserSession.
// =============================================================================

#[handler(variant = "DashboardMetricsRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn dashboard_metrics(
    _req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    Ok(MessageBody::DashboardMetricsResponse(DashboardSnapshot {
        cpu_usage_percent: 0.0,
        ram_used_mb: 0,
        ram_total_mb: 0,
        active_requests: 0,
        total_requests: 0,
        total_errors: 0,
        tokens_per_second: 0,
        active_services: 0,
    }))
}
