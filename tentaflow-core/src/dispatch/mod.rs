// =============================================================================
// Plik: dispatch/mod.rs
// Opis: Runtime support dla proc-macro dispatcher (#[handler] / #[policy] /
//       #[observed]). Definiuje HandlerMeta, SessionAuthKind i Registry.
//       Handlery rejestruja sie przez `inventory::submit!` (generowane przez
//       macro), Registry iteruje po nich przy starcie i buduje variant_name →
//       HandlerMeta mape.
// =============================================================================

use std::collections::HashMap;
use std::sync::OnceLock;

use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth};

pub mod handlers;
pub mod metrics;
pub mod recorder;
pub mod resume_token;
pub mod stream_handlers;
pub mod subscription;

#[cfg(test)]
mod bench;

// =============================================================================
// Kontekst handlera — przekazywany do dispatch_fn
// =============================================================================

/// Informacje o sesji dostarczone handlerowi: kto prosil, jakim sposobem,
/// z jakim correlation_id. Sluzy do identyfikacji usera dla audit/RBAC.
#[derive(Debug, Clone)]
pub struct HandlerContext {
    /// SessionAuth ustalony raz przy WSS handshake.
    pub session: SessionAuth,
    /// Correlation_id dla tracing/spans.
    pub correlation_id: u64,
    /// Connection-scoped resume secret (HMAC key dla resume token verify).
    /// None gdy connection nie ma sekretu (test code).
    pub resume_secret: Option<std::sync::Arc<Vec<u8>>>,
}

impl HandlerContext {
    /// Helper dla testow — buduje minimalny kontekst.
    #[cfg(test)]
    pub fn for_test(session: SessionAuth, correlation_id: u64) -> Self {
        Self {
            session,
            correlation_id,
            resume_secret: None,
        }
    }
}

// =============================================================================
// Rodzaj autoryzacji wymaganej przez handler
// =============================================================================

/// Minimum wymaganej autoryzacji sesji dla wywolania handlera.
/// Porownujemy ORDINAL — wyzszy tier implikuje nizszy (UserSession akceptuje Anonymous fallback? NIE;
/// sprawdzamy DOKLADNA zgodnosc lub wyzszy tier wg tabeli matches()).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAuthKind {
    Anonymous,
    ApiKey,
    UserSession,
    /// Admin = UserSession z rola "admin" w JWT claims. Bootstrap traktuje
    /// rownoznacznie z UserSession — finalna RBAC z claim parsing przyjdzie
    /// w #36 phase 2 razem z auth/session.rs.
    Admin,
    MeshTrust,
}

impl SessionAuthKind {
    /// Czy sesja spelnia minimalny tier wymagany przez handler.
    /// Polityka (bootstrap):
    /// - Anonymous: KAZDA sesja OK (publiczne endpointy, np. ModelList).
    /// - ApiKey: wymaga ApiKey LUB UserSession/Admin LUB MeshTrust.
    /// - UserSession: wymaga UserSession lub Admin.
    /// - Admin: wymaga UserSession (bootstrap) — docelowo z role=admin claim.
    /// - MeshTrust: wymaga MeshTrust (mesh peer-only).
    pub fn session_satisfies(&self, session: &SessionAuth) -> bool {
        match self {
            SessionAuthKind::Anonymous => true,
            SessionAuthKind::ApiKey => matches!(
                session,
                SessionAuth::ApiKey { .. } | SessionAuth::UserSession { .. } | SessionAuth::MeshTrust { .. }
            ),
            SessionAuthKind::UserSession | SessionAuthKind::Admin => {
                matches!(session, SessionAuth::UserSession { .. })
            }
            SessionAuthKind::MeshTrust => matches!(session, SessionAuth::MeshTrust { .. }),
        }
    }
}

// =============================================================================
// HandlerMeta — jeden wpis per handler
// =============================================================================

/// Metadata pojedynczego handlera, rejestrowane przez `#[handler]` macro przez
/// `inventory::submit!`. Linker laczy wszystkie rejestracje w jedna kolekcje.
pub struct HandlerMeta {
    /// Nazwa variantu MessageBody, np. "NodeListRequest".
    pub variant_name: &'static str,
    /// Wersja od ktorej handler jest dostepny (major.minor SemVer).
    pub since_major: u8,
    pub since_minor: u8,
    /// Minimalny tier autoryzacji (z `#[policy(...)]`).
    pub required_auth: SessionAuthKind,
    /// Nazwa metryki Prometheus dla tego handlera.
    pub metric_name: &'static str,
    /// Wskaznik do funkcji handlera. Signatura ustawiona sztywno dla bootstrap.
    pub dispatch_fn: fn(&MessageBody, &HandlerContext) -> Result<MessageBody, ProtocolError>,
}

inventory::collect!(HandlerMeta);

// =============================================================================
// Registry — cache variant_name → HandlerMeta
// =============================================================================

static REGISTRY: OnceLock<HashMap<&'static str, &'static HandlerMeta>> = OnceLock::new();

/// Buduje (lub zwraca cached) rejestr handlerow. Wolane lazy przy pierwszym dispatchu.
fn registry() -> &'static HashMap<&'static str, &'static HandlerMeta> {
    REGISTRY.get_or_init(|| {
        inventory::iter::<HandlerMeta>()
            .map(|h| (h.variant_name, h))
            .collect()
    })
}

/// Wyszukuje handler po nazwie variantu (np. "NodeListRequest").
pub fn find(variant_name: &str) -> Option<&'static HandlerMeta> {
    registry().get(variant_name).copied()
}

/// Liczba zarejestrowanych handlerow (debug/observability).
pub fn handler_count() -> usize {
    registry().len()
}

/// Iterator po wszystkich zarejestrowanych handlerach (dla admin UI / docs).
pub fn all_handlers() -> impl Iterator<Item = &'static HandlerMeta> {
    registry().values().copied()
}

// =============================================================================
// Dispatch helper — glowny entry point dla ws_binary
// =============================================================================

/// Wybiera handler po wariancie MessageBody, sprawdza policy, wola dispatch_fn.
/// Zwraca (response_body, is_error_flag_needed).
pub fn dispatch(
    body: &MessageBody,
    ctx: &HandlerContext,
) -> (MessageBody, bool) {
    let variant_name = variant_name_of(body);
    let Some(handler) = find(variant_name) else {
        return (
            MessageBody::Error(ProtocolError {
                code: ProtocolErrorCode::NotImplemented,
                message: format!("no handler registered for {}", variant_name),
                trace_id: None,
            }),
            true,
        );
    };

    if !handler.required_auth.session_satisfies(&ctx.session) {
        return (
            MessageBody::Error(ProtocolError {
                code: ProtocolErrorCode::PolicyDenied,
                message: format!(
                    "{} requires {:?} session",
                    variant_name, handler.required_auth
                ),
                trace_id: None,
            }),
            true,
        );
    }

    // Opt-in recording: jesli recorder zainicjalizowany (TENTAFLOW_TRACE_WSS=1),
    // zapisuje incoming frame. Wynik trafi na osobny record po dispatchu.
    if let Some(rec) = recorder::global() {
        let body_bytes =
            rkyv::to_bytes::<rkyv::rancor::Error>(body).map(|b| b.to_vec()).unwrap_or_default();
        rec.record(
            recorder::Direction::Incoming,
            ctx.correlation_id,
            0,
            variant_name,
            0,
            &body_bytes,
        );
    }

    let timer = metrics::Timer::start(handler.variant_name);
    let result = match (handler.dispatch_fn)(body, ctx) {
        Ok(response) => {
            let is_err = matches!(response, MessageBody::Error(_));
            (response, is_err)
        }
        Err(err) => (MessageBody::Error(err), true),
    };
    timer.finish(result.1);

    if let Some(rec) = recorder::global() {
        let body_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&result.0)
            .map(|b| b.to_vec())
            .unwrap_or_default();
        let resp_variant_name = variant_name_of(&result.0);
        let flags: u8 = if result.1 { 0b0000_0001 } else { 0 };
        rec.record(
            recorder::Direction::Outgoing,
            ctx.correlation_id,
            0,
            resp_variant_name,
            flags,
            &body_bytes,
        );
    }

    result
}

/// Mapuje MessageBody enum discriminant na string nazwe wariantu. Musi byc
/// zgodne z nazwami przekazywanymi do `#[handler(variant = "...")]`.
/// Pub(crate) zeby ws_binary mogl uzyc dla streaming dispatch.
pub fn variant_name_of(body: &MessageBody) -> &'static str {
    match body {
        MessageBody::MetaSchemaVersionCheck { .. } => "MetaSchemaVersionCheck",
        MessageBody::MetaSchemaVersionAck { .. } => "MetaSchemaVersionAck",
        MessageBody::MetaHeartbeat { .. } => "MetaHeartbeat",
        MessageBody::MetaCancelStream => "MetaCancelStream",
        MessageBody::NodeListRequest => "NodeListRequest",
        MessageBody::NodeListResponse { .. } => "NodeListResponse",
        MessageBody::ModelListRequest => "ModelListRequest",
        MessageBody::ModelListResponse { .. } => "ModelListResponse",
        MessageBody::NodeInfoRequest { .. } => "NodeInfoRequest",
        MessageBody::ApiKeyListRequest => "ApiKeyListRequest",
        MessageBody::ApiKeyListResponse { .. } => "ApiKeyListResponse",
        MessageBody::ApiKeyCreateRequestBody(_) => "ApiKeyCreateRequest",
        MessageBody::ApiKeyCreateResponseBody(_) => "ApiKeyCreateResponse",
        MessageBody::ApiKeyRevokeRequest { .. } => "ApiKeyRevokeRequest",
        MessageBody::ApiKeyRevokeResponse { .. } => "ApiKeyRevokeResponse",
        MessageBody::AuthLoginRequestBody(_) => "AuthLoginRequest",
        MessageBody::AuthLoginResponseBody(_) => "AuthLoginResponse",
        MessageBody::AuthMeRequest => "AuthMeRequest",
        MessageBody::AuthMeResponseBody(_) => "AuthMeResponse",
        MessageBody::ChatStreamRequestBody(_) => "ChatStreamRequest",
        MessageBody::ChatStreamChunkBody(_) => "ChatStreamChunk",
        MessageBody::ChatStreamEndBody(_) => "ChatStreamEnd",
        MessageBody::ClusterUpdateRequestBody(_) => "ClusterUpdateRequest",
        MessageBody::ClusterUpdateResponseBody(_) => "ClusterUpdateResponse",
        MessageBody::MeshPeersListRequest => "MeshPeersListRequest",
        MessageBody::MeshPeersListResponse { .. } => "MeshPeersListResponse",
        MessageBody::MeshPairInitRequestBody(_) => "MeshPairInitRequest",
        MessageBody::MeshPairInitResponseBody(_) => "MeshPairInitResponse",
        MessageBody::MeshTrustRevoked(_) => "MeshTrustRevoked",
        MessageBody::MeshTrustedKeysSync(_) => "MeshTrustedKeysSync",
        MessageBody::SubscribeResumeRequest { .. } => "SubscribeResumeRequest",
        MessageBody::SubscribeResumeAck { .. } => "SubscribeResumeAck",
        MessageBody::SubscribeResumeOffer { .. } => "SubscribeResumeOffer",
        MessageBody::ModelDetailRequest { .. } => "ModelDetailRequest",
        MessageBody::ModelDetailResponse(_) => "ModelDetailResponse",
        MessageBody::ModelInstallRequestBody(_) => "ModelInstallRequest",
        MessageBody::ModelInstallResponse { .. } => "ModelInstallResponse",
        MessageBody::ModelDeleteRequest { .. } => "ModelDeleteRequest",
        MessageBody::ModelDeleteResponse { .. } => "ModelDeleteResponse",
        MessageBody::HubEngineListRequest => "HubEngineListRequest",
        MessageBody::HubEngineListResponse { .. } => "HubEngineListResponse",
        MessageBody::HubModelSearchRequest { .. } => "HubModelSearchRequest",
        MessageBody::HubModelSearchResponse { .. } => "HubModelSearchResponse",
        MessageBody::HubDownloadProgressBody(_) => "HubDownloadProgress",
        MessageBody::FlowListRequest => "FlowListRequest",
        MessageBody::FlowListResponse { .. } => "FlowListResponse",
        MessageBody::FlowDetailRequest { .. } => "FlowDetailRequest",
        MessageBody::FlowDetailResponse(_) => "FlowDetailResponse",
        MessageBody::FlowCreateRequestBody(_) => "FlowCreateRequest",
        MessageBody::FlowCreateResponse { .. } => "FlowCreateResponse",
        MessageBody::FlowDeleteRequest { .. } => "FlowDeleteRequest",
        MessageBody::FlowDeleteResponse { .. } => "FlowDeleteResponse",
        MessageBody::FlowExecutionsListRequest { .. } => "FlowExecutionsListRequest",
        MessageBody::FlowExecutionsListResponse { .. } => "FlowExecutionsListResponse",
        MessageBody::ServiceListRequest => "ServiceListRequest",
        MessageBody::ServiceListResponse { .. } => "ServiceListResponse",
        MessageBody::ServiceDeployRequestBody(_) => "ServiceDeployRequest",
        MessageBody::ServiceDeployAccepted { .. } => "ServiceDeployAccepted",
        MessageBody::ServiceDeployProgressBody(_) => "ServiceDeployProgress",
        MessageBody::ServiceStopRequest { .. } => "ServiceStopRequest",
        MessageBody::ServiceStopResponse { .. } => "ServiceStopResponse",
        MessageBody::PromptListRequest => "PromptListRequest",
        MessageBody::PromptListResponse { .. } => "PromptListResponse",
        MessageBody::PromptDetailRequest { .. } => "PromptDetailRequest",
        MessageBody::PromptDetailResponse(_) => "PromptDetailResponse",
        MessageBody::RegistryListRequest => "RegistryListRequest",
        MessageBody::RegistryListResponse { .. } => "RegistryListResponse",
        MessageBody::AuditEventBody(_) => "AuditEvent",
        MessageBody::ContainerListRequest => "ContainerListRequest",
        MessageBody::ContainerListResponse { .. } => "ContainerListResponse",
        MessageBody::ContainerStartRequest { .. } => "ContainerStartRequest",
        MessageBody::ContainerStartResponse { .. } => "ContainerStartResponse",
        MessageBody::ContainerStopRequest { .. } => "ContainerStopRequest",
        MessageBody::ContainerStopResponse { .. } => "ContainerStopResponse",
        MessageBody::ContainerLogStreamRequest { .. } => "ContainerLogStreamRequest",
        MessageBody::ContainerLogChunkBody(_) => "ContainerLogChunk",
        MessageBody::VoiceProfileListRequest => "VoiceProfileListRequest",
        MessageBody::VoiceProfileListResponse { .. } => "VoiceProfileListResponse",
        MessageBody::TtsRuleListRequest => "TtsRuleListRequest",
        MessageBody::TtsRuleListResponse { .. } => "TtsRuleListResponse",
        MessageBody::TtsRuleCreateRequest(_) => "TtsRuleCreateRequest",
        MessageBody::TtsRuleCreateResponse { .. } => "TtsRuleCreateResponse",
        MessageBody::TtsRuleDeleteRequest { .. } => "TtsRuleDeleteRequest",
        MessageBody::TtsRuleDeleteResponse { .. } => "TtsRuleDeleteResponse",
        MessageBody::PiiRuleListRequest => "PiiRuleListRequest",
        MessageBody::PiiRuleListResponse { .. } => "PiiRuleListResponse",
        MessageBody::FastPathListRequest => "FastPathListRequest",
        MessageBody::FastPathListResponse { .. } => "FastPathListResponse",
        MessageBody::SettingsListRequest => "SettingsListRequest",
        MessageBody::SettingsListResponse { .. } => "SettingsListResponse",
        MessageBody::SettingsUpdateRequestBody(_) => "SettingsUpdateRequest",
        MessageBody::SettingsUpdateResponse { .. } => "SettingsUpdateResponse",
        MessageBody::DashboardMetricsRequest => "DashboardMetricsRequest",
        MessageBody::DashboardMetricsResponse(_) => "DashboardMetricsResponse",
        MessageBody::Error(_) => "Error",
    }
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_auth_kind_anonymous_accepts_all() {
        assert!(SessionAuthKind::Anonymous.session_satisfies(&SessionAuth::Anonymous));
        assert!(SessionAuthKind::Anonymous.session_satisfies(&SessionAuth::UserSession {
            user_id: [0u8; 16]
        }));
    }

    #[test]
    fn session_auth_kind_user_session_requires_exact_match() {
        let kind = SessionAuthKind::UserSession;
        assert!(kind.session_satisfies(&SessionAuth::UserSession { user_id: [0u8; 16] }));
        assert!(!kind.session_satisfies(&SessionAuth::Anonymous));
        assert!(!kind.session_satisfies(&SessionAuth::ApiKey {
            key_id: "x".to_string()
        }));
        assert!(!kind.session_satisfies(&SessionAuth::MeshTrust {
            node_id: [0u8; 32],
            epoch: 0
        }));
    }

    #[test]
    fn session_auth_kind_apikey_accepts_higher_tiers() {
        let kind = SessionAuthKind::ApiKey;
        assert!(kind.session_satisfies(&SessionAuth::ApiKey {
            key_id: "x".to_string()
        }));
        assert!(kind.session_satisfies(&SessionAuth::UserSession { user_id: [0u8; 16] }));
        assert!(kind.session_satisfies(&SessionAuth::MeshTrust {
            node_id: [0u8; 32],
            epoch: 0
        }));
        assert!(!kind.session_satisfies(&SessionAuth::Anonymous));
    }

    #[test]
    fn session_auth_kind_mesh_trust_requires_exact() {
        let kind = SessionAuthKind::MeshTrust;
        assert!(kind.session_satisfies(&SessionAuth::MeshTrust {
            node_id: [0u8; 32],
            epoch: 0
        }));
        assert!(!kind.session_satisfies(&SessionAuth::Anonymous));
        assert!(!kind.session_satisfies(&SessionAuth::UserSession { user_id: [0u8; 16] }));
    }

    #[test]
    fn dispatch_unknown_variant_returns_not_implemented() {
        // Variants are all known by variant_name_of, ale jesli handler nie
        // zarejestrowany w registry (np. Error) zwraca NotImplemented.
        // NOTE: ten test zalezy od tego ze handler dla Error nie istnieje
        // (bo Error to output variant, nie input).
        let ctx = HandlerContext {
            session: SessionAuth::UserSession { user_id: [0u8; 16] },
            correlation_id: 1,
            resume_secret: None,
        };
        let body = MessageBody::Error(ProtocolError {
            code: ProtocolErrorCode::Internal,
            message: "test".to_string(),
            trace_id: None,
        });
        let (resp, is_err) = dispatch(&body, &ctx);
        assert!(is_err);
        match resp {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::NotImplemented),
            _ => panic!("expected error"),
        }
    }

    #[test]
    fn registry_is_populated_with_bootstrap_handlers() {
        let count = handler_count();
        assert!(
            count >= 10,
            "expected at least 10 handlers registered (bootstrap #26 + expanded #36), got {}",
            count
        );
        assert!(find("NodeListRequest").is_some());
        assert!(find("ModelListRequest").is_some());
        assert!(find("ApiKeyListRequest").is_some());
        assert!(find("AuthLoginRequest").is_some());
        assert!(find("ChatStreamRequest").is_some());
        assert!(find("ClusterUpdateRequest").is_some());
    }

    #[test]
    fn dispatch_covers_all_seven_archetypes() {
        use tentaflow_protocol::{
            ApiKeyCreateRequest, AuthLoginRequest, ChatMessage, ChatStreamRequest,
            ClusterUpdateRequest,
        };
        let ctx_user = HandlerContext {
            session: SessionAuth::UserSession { user_id: [0u8; 16] },
            correlation_id: 100,
            resume_secret: None,
        };

        let r_list = dispatch(&MessageBody::ApiKeyListRequest, &ctx_user);
        assert!(!r_list.1);
        assert!(matches!(r_list.0, MessageBody::ApiKeyListResponse { .. }));

        let r_one = dispatch(&MessageBody::AuthMeRequest, &ctx_user);
        assert!(!r_one.1);
        assert!(matches!(r_one.0, MessageBody::AuthMeResponseBody(_)));

        let r_stream = dispatch(
            &MessageBody::ChatStreamRequestBody(ChatStreamRequest {
                model_id: "x".to_string(),
                messages: vec![ChatMessage {
                    role: "user".to_string(),
                    content: "hi".to_string(),
                }],
                temperature: None,
                max_tokens: None,
            }),
            &ctx_user,
        );
        assert!(!r_stream.1);
        assert!(matches!(r_stream.0, MessageBody::ChatStreamEndBody(_)));

        let w_create = dispatch(
            &MessageBody::ApiKeyCreateRequestBody(ApiKeyCreateRequest {
                name: "svc".to_string(),
                scopes: vec![],
            }),
            &ctx_user,
        );
        assert!(!w_create.1);
        assert!(matches!(w_create.0, MessageBody::ApiKeyCreateResponseBody(_)));

        let w_update = dispatch(
            &MessageBody::ClusterUpdateRequestBody(ClusterUpdateRequest {
                cluster_id: "c1".to_string(),
                name: "Prod".to_string(),
                description: None,
            }),
            &ctx_user,
        );
        assert!(!w_update.1);
        assert!(matches!(w_update.0, MessageBody::ClusterUpdateResponseBody(_)));

        let w_delete = dispatch(
            &MessageBody::ApiKeyRevokeRequest {
                key_id: "x".to_string(),
            },
            &ctx_user,
        );
        assert!(!w_delete.1);
        assert!(matches!(w_delete.0, MessageBody::ApiKeyRevokeResponse { .. }));

        let w_action = dispatch(
            &MessageBody::AuthLoginRequestBody(AuthLoginRequest {
                username: "u".to_string(),
                password: "p".to_string(),
            }),
            &HandlerContext {
                session: SessionAuth::Anonymous,
                correlation_id: 1,
                resume_secret: None,
            },
        );
        assert!(!w_action.1);
        assert!(matches!(w_action.0, MessageBody::AuthLoginResponseBody(_)));
    }

    #[test]
    fn dispatch_node_list_request_via_registry() {
        let ctx = HandlerContext {
            session: SessionAuth::UserSession { user_id: [0u8; 16] },
            correlation_id: 7,
            resume_secret: None,
        };
        let (resp, is_err) = dispatch(&MessageBody::NodeListRequest, &ctx);
        assert!(!is_err);
        assert!(matches!(resp, MessageBody::NodeListResponse { .. }));
    }

    #[test]
    fn dispatch_policy_denies_anonymous_for_node_list() {
        let ctx = HandlerContext {
            session: SessionAuth::Anonymous,
            correlation_id: 8,
            resume_secret: None,
        };
        let (resp, is_err) = dispatch(&MessageBody::NodeListRequest, &ctx);
        assert!(is_err);
        match resp {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::PolicyDenied),
            _ => panic!("expected PolicyDenied error"),
        }
    }

    #[test]
    fn dispatch_model_list_allows_anonymous() {
        let ctx = HandlerContext {
            session: SessionAuth::Anonymous,
            correlation_id: 9,
            resume_secret: None,
        };
        let (resp, is_err) = dispatch(&MessageBody::ModelListRequest, &ctx);
        assert!(!is_err);
        assert!(matches!(resp, MessageBody::ModelListResponse { .. }));
    }
}
