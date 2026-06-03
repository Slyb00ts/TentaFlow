// =============================================================================
// Plik: dispatch/mod.rs
// Opis: Runtime support dla proc-macro dispatcher (#[handler] / #[policy] /
//       #[observed]). Definiuje HandlerMeta, SessionAuthKind i Registry.
//       Handlery rejestruja sie przez `inventory::submit!` (generowane przez
//       macro), Registry iteruje po nich przy starcie i buduje variant_name →
//       HandlerMeta mape.
// =============================================================================

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth};

/// Boxed future zwracany przez handler. Zunifikowana signatura:
/// sync handlery sa transparentnie owijane przez makro w `async move { ... }`.
pub type HandlerFuture<'a> =
    Pin<Box<dyn Future<Output = Result<MessageBody, ProtocolError>> + Send + 'a>>;

/// Typ pointera rejestrowany w HandlerMeta. Kazdy handler (sync lub async)
/// przez makro produkuje funkcje o tej signaturze zwracajaca boxed future.
pub type HandlerDispatchFn = for<'a> fn(&'a MessageBody, &'a HandlerContext) -> HandlerFuture<'a>;

pub mod addon_perm_broadcast;
pub mod audit_broadcast;
#[cfg(feature = "camera")]
pub mod camera_admin;
pub mod compliance_admin;
pub mod handlers;
pub mod legal_admin;
pub mod meeting_live_broadcast;
pub mod mesh_write_handlers;
pub mod metrics;
pub mod recorder;
pub mod resume_token;
pub mod role_catalog;
pub mod state;
pub mod stream;
pub mod stream_handlers;
pub mod subscription;
pub mod system_event_broadcast;
pub mod ui_cbor_broadcast;
pub mod ui_channel;

pub use state::AppState;

#[cfg(test)]
mod bench;

// =============================================================================
// Kontekst handlera — przekazywany do dispatch_fn
// =============================================================================

/// Informacje o sesji dostarczone handlerowi: kto prosil, jakim sposobem,
/// z jakim correlation_id, plus shared AppState dla dostepu do DB/Router/itd.
#[derive(Clone)]
pub struct HandlerContext {
    /// SessionAuth ustalony raz przy WSS handshake.
    pub session: SessionAuth,
    /// Correlation_id dla tracing/spans.
    pub correlation_id: u64,
    /// Per-WS-connection unique identifier. Used as a key in the UI session
    /// registry so panel lifecycle state is scoped to the originating socket.
    pub connection_id: u64,
    /// Connection-scoped resume secret (HMAC key dla resume token verify).
    pub resume_secret: Option<std::sync::Arc<Vec<u8>>>,
    /// Shared resources serwera (DB, Router, MeshPeerStore, ...).
    pub state: std::sync::Arc<state::AppState>,
    /// F2 P1.b — per-request org snapshot. `None` for paths without a user
    /// session (mesh handlers, anonymous endpoints, system-initiated calls);
    /// handlers that need org scoping must check this is `Some` and reject
    /// otherwise. Threaded in by the WS binary entrypoint after session
    /// resolve; mesh / test handlers leave it `None`.
    pub org_context: Option<crate::services::rbac::OrgContext>,
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
    /// PowerUser = UserSession z rola "power_user" LUB "admin".
    /// Flow Builder, reguly TTS/PII, prompty.
    PowerUser,
    /// Admin = UserSession z rola "admin".
    Admin,
    MeshTrust,
}

impl SessionAuthKind {
    /// Czy sesja spelnia minimalny tier wymagany przez handler.
    /// Polityka:
    /// - Anonymous: KAZDA sesja OK (publiczne endpointy, np. ModelList).
    /// - ApiKey: wymaga ApiKey LUB UserSession LUB MeshTrust.
    /// - UserSession: wymaga UserSession (dowolny role).
    /// - PowerUser: wymaga UserSession z role="power_user" albo "admin".
    /// - Admin: wymaga UserSession z role="admin" (Zero Trust, role z DB).
    /// - MeshTrust: wymaga MeshTrust (mesh peer-only).
    pub fn session_satisfies(&self, session: &SessionAuth) -> bool {
        match self {
            SessionAuthKind::Anonymous => true,
            SessionAuthKind::ApiKey => matches!(
                session,
                SessionAuth::ApiKey { .. }
                    | SessionAuth::UserSession { .. }
                    | SessionAuth::MeshTrust { .. }
            ),
            SessionAuthKind::UserSession => matches!(session, SessionAuth::UserSession { .. }),
            SessionAuthKind::PowerUser => matches!(
                session,
                SessionAuth::UserSession { role: Some(r), .. } if r == "admin" || r == "power_user"
            ),
            SessionAuthKind::Admin => matches!(
                session,
                SessionAuth::UserSession { role: Some(r), .. } if r == "admin"
            ),
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
    /// Nazwa variantu MessageBody, np. "ModelListRequest".
    pub variant_name: &'static str,
    /// Wersja od ktorej handler jest dostepny (major.minor SemVer).
    pub since_major: u8,
    pub since_minor: u8,
    /// Minimalny tier autoryzacji (z `#[policy(...)]`).
    pub required_auth: SessionAuthKind,
    /// Nazwa metryki dla tego handlera.
    pub metric_name: &'static str,
    /// Wskaznik do funkcji handlera. Zunifikowana async signatura — sync
    /// handlery sa owijane przez makro w `Box::pin(async move { ... })`.
    pub dispatch_fn: HandlerDispatchFn,
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

/// Wyszukuje handler po nazwie variantu (np. "ModelListRequest").
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
/// Zwraca (response_body, is_error_flag_needed). Signatura jest async —
/// sync handlery sa owijane w `async move` przez makro `#[handler]`.
pub async fn dispatch(body: &MessageBody, ctx: &HandlerContext) -> (MessageBody, bool) {
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
    // zapisuje incoming frame. P1 FIX: dla sensitive variantow body jest empty
    // (variant name + correlation_id wystarczaja do audit, tresc by exposowala
    // hasla/tokeny do SQLite recordera).
    if let Some(rec) = recorder::global() {
        let body_bytes = if is_sensitive_variant(body) {
            Vec::new()
        } else {
            tentaflow_protocol::cbor::encode(body).unwrap_or_default()
        };
        let flags: u8 = if is_sensitive_variant(body) {
            0b1000_0000
        } else {
            0
        };
        rec.record(
            recorder::Direction::Incoming,
            ctx.correlation_id,
            0,
            variant_name,
            flags,
            &body_bytes,
        );
    }

    let timer = metrics::Timer::start(handler.variant_name);
    let fut = (handler.dispatch_fn)(body, ctx);
    let result = match fut.await {
        Ok(response) => {
            let is_err = matches!(response, MessageBody::Error(_));
            (response, is_err)
        }
        Err(err) => (MessageBody::Error(err), true),
    };
    timer.finish(result.1);

    if let Some(rec) = recorder::global() {
        let body_bytes = if is_sensitive_variant(&result.0) {
            Vec::new()
        } else {
            tentaflow_protocol::cbor::encode(&result.0).unwrap_or_default()
        };
        let resp_variant_name = variant_name_of(&result.0);
        // Bit 0 = is_error, bit 7 = body_redacted (sensitive variant)
        let mut flags: u8 = if result.1 { 0b0000_0001 } else { 0 };
        if is_sensitive_variant(&result.0) {
            flags |= 0b1000_0000;
        }
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

/// Czy body tego variantu zawiera sekrety ktorych NIE wolno logowac do recordera.
/// Recorder w dispatch::dispatch() sprawdza te funkcje; gdy true, zapisuje pusty
/// body + flag marker zeby audit wiedzial ze byl frame, ale tresci nie ma.
///
/// Secrets list (P1 fix):
///   - AuthLoginRequest: plaintext password
///   - AuthLoginResponse: JWT token (short-lived, ale still bearer)
///   - ApiKeyCreateResponse: plaintext "shown only once" token
///   - SettingsUpdateRequest: potencjalnie is_secret=true entries
fn is_sensitive_variant(body: &MessageBody) -> bool {
    if matches!(
        body,
        MessageBody::AuthLoginRequestBody(_)
            | MessageBody::AuthLoginResponseBody(_)
            | MessageBody::ApiKeyCreateResponseBody(_)
            | MessageBody::SettingsUpdateRequestBody(_)
            | MessageBody::AddonConfigSetRequestBody(_)
            | MessageBody::AddonInstallRequestBody(_)
    ) {
        return true;
    }
    // CameraAdminBody:AddOnvifRequest carries plaintext ONVIF user+password.
    if matches!(
        body,
        MessageBody::CameraAdminBody(tentaflow_protocol::CameraAdminPayload::AddOnvifRequest(_))
    ) {
        return true;
    }
    // CameraAdminBody:FrameUrlResponse carries a signed `/frames/<ref>?token=...`
    // URL whose query string includes a live HMAC token. With
    // `TENTAFLOW_TRACE_WSS=1` an attacker reading trace logs could lift the
    // token wire and replay the download within the (up to 300 s) TTL.
    if matches!(
        body,
        MessageBody::CameraAdminBody(tentaflow_protocol::CameraAdminPayload::FrameUrlResponse(_))
    ) {
        return true;
    }
    // LegalAdminBody:GenerateResponse carries a fully-formed signed download
    // URL whose query string includes a live HMAC token. With
    // `TENTAFLOW_TRACE_WSS=1` an attacker reading trace logs could lift the
    // token wire and replay the download within the 1 h TTL.
    matches!(
        body,
        MessageBody::LegalAdminBody(tentaflow_protocol::LegalAdminPayload::GenerateResponse(_))
    )
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
        MessageBody::ModelListRequest => "ModelListRequest",
        MessageBody::ModelListResponse { .. } => "ModelListResponse",
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
        MessageBody::MePreferencesGetRequestBody(_) => "MePreferencesGetRequest",
        MessageBody::MePreferencesGetResponseBody(_) => "MePreferencesGetResponse",
        MessageBody::MePreferencesUpdateRequestBody(_) => "MePreferencesUpdateRequest",
        MessageBody::MePreferencesUpdateResponseBody(_) => "MePreferencesUpdateResponse",
        MessageBody::ChatStreamRequestBody(_) => "ChatStreamRequest",
        MessageBody::ChatStreamChunkBody(_) => "ChatStreamChunk",
        MessageBody::ChatStreamEndBody(_) => "ChatStreamEnd",
        MessageBody::FlowInvokeRequestBody(_) => "FlowInvokeRequest",
        MessageBody::FlowInvokeChunkBody(_) => "FlowInvokeChunk",
        MessageBody::FlowInvokeEndBody(_) => "FlowInvokeEnd",
        MessageBody::TranslateBody(p) => match p {
            tentaflow_protocol::TranslatePayload::Req(_) => "TranslateRequest",
            tentaflow_protocol::TranslatePayload::Res(_) => "TranslateResponse",
        },
        MessageBody::ClusterListRequest => "ClusterListRequest",
        MessageBody::ClusterListResponseBody(_) => "ClusterListResponse",
        MessageBody::ClusterDetailRequestBody(_) => "ClusterDetailRequest",
        MessageBody::ClusterDetailResponseBody(_) => "ClusterDetailResponse",
        MessageBody::ClusterCreateRequestBody(_) => "ClusterCreateRequest",
        MessageBody::ClusterCreateResponseBody(_) => "ClusterCreateResponse",
        MessageBody::ClusterUpdateRequestBody(_) => "ClusterUpdateRequest",
        MessageBody::ClusterUpdateResponseBody(_) => "ClusterUpdateResponse",
        MessageBody::ClusterDeleteRequestBody(_) => "ClusterDeleteRequest",
        MessageBody::ClusterDeleteResponseBody(_) => "ClusterDeleteResponse",
        MessageBody::ClusterAddMemberRequestBody(_) => "ClusterAddMemberRequest",
        MessageBody::ClusterAddMemberResponseBody(_) => "ClusterAddMemberResponse",
        MessageBody::ClusterRemoveMemberRequestBody(_) => "ClusterRemoveMemberRequest",
        MessageBody::ClusterRemoveMemberResponseBody(_) => "ClusterRemoveMemberResponse",
        MessageBody::ClusterProbeStreamRequestBody(_) => "ClusterProbeStreamRequest",
        MessageBody::ClusterProbeStreamChunkBody(_) => "ClusterProbeStreamChunk",
        MessageBody::ClusterProbeStreamEndBody(_) => "ClusterProbeStreamEnd",
        MessageBody::MeshPeersListRequest => "MeshPeersListRequest",
        MessageBody::MeshPeersListResponse { .. } => "MeshPeersListResponse",
        MessageBody::MeshPairInitRequestBody(_) => "MeshPairInitRequest",
        MessageBody::MeshPairInitResponseBody(_) => "MeshPairInitResponse",
        MessageBody::MeshTrustEventBody(p) => match p {
            tentaflow_protocol::MeshTrustEventPayload::Revoked(_) => "MeshTrustRevoked",
            tentaflow_protocol::MeshTrustEventPayload::KeysSync(_) => "MeshTrustedKeysSync",
        },
        MessageBody::ProfilingBody(p) => match p {
            tentaflow_protocol::ProfilingPayload::StartRequest(_) => "ProfilingStartRequest",
            tentaflow_protocol::ProfilingPayload::StartResponse(_) => "ProfilingStartResponse",
            tentaflow_protocol::ProfilingPayload::StopRequest(_) => "ProfilingStopRequest",
            tentaflow_protocol::ProfilingPayload::StopResponse(_) => "ProfilingStopResponse",
            tentaflow_protocol::ProfilingPayload::SessionsRequest(_) => "ProfilingSessionsRequest",
            tentaflow_protocol::ProfilingPayload::SessionsResponse(_) => {
                "ProfilingSessionsResponse"
            }
            tentaflow_protocol::ProfilingPayload::ReportRequest(_) => "ProfilingReportRequest",
            tentaflow_protocol::ProfilingPayload::ReportResponse(_) => "ProfilingReportResponse",
            tentaflow_protocol::ProfilingPayload::DeleteRequest(_) => "ProfilingDeleteRequest",
            tentaflow_protocol::ProfilingPayload::DeleteResponse(_) => "ProfilingDeleteResponse",
            tentaflow_protocol::ProfilingPayload::DownloadRequest(_) => "ProfilingDownloadRequest",
            tentaflow_protocol::ProfilingPayload::DownloadResponse(_) => {
                "ProfilingDownloadResponse"
            }
            tentaflow_protocol::ProfilingPayload::ActiveInfoRequest(_) => {
                "ProfilingActiveInfoRequest"
            }
            tentaflow_protocol::ProfilingPayload::ActiveInfoResponse(_) => {
                "ProfilingActiveInfoResponse"
            }
            tentaflow_protocol::ProfilingPayload::ValidateSudoRequest(_) => {
                "ProfilingValidateSudoRequest"
            }
            tentaflow_protocol::ProfilingPayload::ValidateSudoResponse(_) => {
                "ProfilingValidateSudoResponse"
            }
            tentaflow_protocol::ProfilingPayload::CollectorsStatusRequest(_) => {
                "ProfilingCollectorsStatusRequest"
            }
            tentaflow_protocol::ProfilingPayload::CollectorsStatusResponse(_) => {
                "ProfilingCollectorsStatusResponse"
            }
        },
        MessageBody::CameraAdminBody(p) => match p {
            tentaflow_protocol::CameraAdminPayload::DiscoverRequest(_) => "CameraDiscoverRequest",
            tentaflow_protocol::CameraAdminPayload::DiscoverResponse(_) => "CameraDiscoverResponse",
            tentaflow_protocol::CameraAdminPayload::AddOnvifRequest(_) => "CameraAddOnvifRequest",
            tentaflow_protocol::CameraAdminPayload::AddOnvifResponse(_) => "CameraAddOnvifResponse",
            tentaflow_protocol::CameraAdminPayload::FrameUrlRequest(_) => "CameraFrameUrlRequest",
            tentaflow_protocol::CameraAdminPayload::FrameUrlResponse(_) => "CameraFrameUrlResponse",
        },
        MessageBody::LegalAdminBody(p) => match p {
            tentaflow_protocol::LegalAdminPayload::ListRequest(_) => "LegalDocumentsListRequest",
            tentaflow_protocol::LegalAdminPayload::ListResponse(_) => "LegalDocumentsListResponse",
            tentaflow_protocol::LegalAdminPayload::GenerateRequest(_) => {
                "LegalDocumentGenerateRequest"
            }
            tentaflow_protocol::LegalAdminPayload::GenerateResponse(_) => {
                "LegalDocumentGenerateResponse"
            }
            tentaflow_protocol::LegalAdminPayload::RevokeRequest(_) => "LegalDocumentRevokeRequest",
            tentaflow_protocol::LegalAdminPayload::RevokeResponse(_) => {
                "LegalDocumentRevokeResponse"
            }
        },
        MessageBody::ComplianceAdminBody(p) => match p {
            tentaflow_protocol::ComplianceAdminPayload::ListDataCategoriesRequest => {
                "ComplianceDataCategoriesListRequest"
            }
            tentaflow_protocol::ComplianceAdminPayload::ListDataCategoriesResponse { .. } => {
                "ComplianceDataCategoriesListResponse"
            }
            tentaflow_protocol::ComplianceAdminPayload::ListRetentionPoliciesRequest => {
                "ComplianceRetentionPoliciesListRequest"
            }
            tentaflow_protocol::ComplianceAdminPayload::ListRetentionPoliciesResponse {
                ..
            } => "ComplianceRetentionPoliciesListResponse",
            tentaflow_protocol::ComplianceAdminPayload::ListAiEventsRequest(_) => {
                "ComplianceAiEventsListRequest"
            }
            tentaflow_protocol::ComplianceAdminPayload::ListAiEventsResponse { .. } => {
                "ComplianceAiEventsListResponse"
            }
        },
        MessageBody::StreamBody(p) => match p {
            tentaflow_protocol::StreamPayload::SubscribeRequest(_) => "StreamSubscribeRequest",
            tentaflow_protocol::StreamPayload::SubscribeResponse(_) => "StreamSubscribeResponse",
            tentaflow_protocol::StreamPayload::Frame(_) => "StreamFrame",
            tentaflow_protocol::StreamPayload::CloseRequest(_) => "StreamCloseRequest",
            tentaflow_protocol::StreamPayload::Closed(_) => "StreamClosed",
        },
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
        MessageBody::SchedulerBody(p) => match p {
            tentaflow_protocol::SchedulerPayload::JobsListRequest(_) => "SchedulerJobsListRequest",
            tentaflow_protocol::SchedulerPayload::JobsListResponse(_) => {
                "SchedulerJobsListResponse"
            }
            tentaflow_protocol::SchedulerPayload::ActionsListRequest(_) => {
                "SchedulerActionsListRequest"
            }
            tentaflow_protocol::SchedulerPayload::ActionsListResponse(_) => {
                "SchedulerActionsListResponse"
            }
            tentaflow_protocol::SchedulerPayload::RunsListRequest(_) => "SchedulerRunsListRequest",
            tentaflow_protocol::SchedulerPayload::RunsListResponse(_) => {
                "SchedulerRunsListResponse"
            }
            tentaflow_protocol::SchedulerPayload::JobUpsertRequest(_) => {
                "SchedulerJobUpsertRequest"
            }
            tentaflow_protocol::SchedulerPayload::JobUpsertResponse(_) => {
                "SchedulerJobUpsertResponse"
            }
            tentaflow_protocol::SchedulerPayload::JobDeleteRequest(_) => {
                "SchedulerJobDeleteRequest"
            }
            tentaflow_protocol::SchedulerPayload::JobDeleteResponse(_) => {
                "SchedulerJobDeleteResponse"
            }
            tentaflow_protocol::SchedulerPayload::JobRunNowRequest(_) => {
                "SchedulerJobRunNowRequest"
            }
            tentaflow_protocol::SchedulerPayload::JobRunNowResponse(_) => {
                "SchedulerJobRunNowResponse"
            }
        },
        MessageBody::SyncConflictBody(p) => match p {
            tentaflow_protocol::SyncConflictPayload::ListRequest(_) => "SyncConflictsListRequest",
            tentaflow_protocol::SyncConflictPayload::ListResponse(_) => "SyncConflictsListResponse",
            tentaflow_protocol::SyncConflictPayload::ResolveRequest(_) => {
                "SyncConflictResolveRequest"
            }
            tentaflow_protocol::SyncConflictPayload::ResolveResponse(_) => {
                "SyncConflictResolveResponse"
            }
        },
        MessageBody::SyncStorageBody(p) => match p {
            tentaflow_protocol::SyncStoragePayload::ReportRequest(_) => "SyncStorageReportRequest",
            tentaflow_protocol::SyncStoragePayload::ReportResponse(_) => {
                "SyncStorageReportResponse"
            }
        },
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
        MessageBody::PromptListRequest => "PromptListRequest",
        MessageBody::PromptListResponse { .. } => "PromptListResponse",
        MessageBody::PromptDetailRequest { .. } => "PromptDetailRequest",
        MessageBody::PromptDetailResponse(_) => "PromptDetailResponse",
        MessageBody::NotesRequestBody(r) => match r {
            tentaflow_protocol::NotesRequest::List(_) => "NotesListRequest",
            tentaflow_protocol::NotesRequest::Detail(_) => "NoteDetailRequest",
            tentaflow_protocol::NotesRequest::Create(_) => "NoteCreateRequest",
            tentaflow_protocol::NotesRequest::Update(_) => "NoteUpdateRequest",
            tentaflow_protocol::NotesRequest::SetPinned(_) => "NoteSetPinnedRequest",
            tentaflow_protocol::NotesRequest::Delete(_) => "NoteDeleteRequest",
        },
        MessageBody::NotesResponseBody(r) => match r {
            tentaflow_protocol::NotesResponse::List(_) => "NotesListResponse",
            tentaflow_protocol::NotesResponse::Detail(_) => "NoteDetailResponse",
            tentaflow_protocol::NotesResponse::Create(_) => "NoteCreateResponse",
            tentaflow_protocol::NotesResponse::Update(_) => "NoteUpdateResponse",
            tentaflow_protocol::NotesResponse::SetPinned(_) => "NoteSetPinnedResponse",
            tentaflow_protocol::NotesResponse::Delete(_) => "NoteDeleteResponse",
        },
        MessageBody::DeploymentBody(p) => match p {
            tentaflow_protocol::DeploymentPayload::ReqStart(_) => "ServiceManifestDeployRequest",
            tentaflow_protocol::DeploymentPayload::ResStart(_) => "ServiceManifestDeployResponse",
            tentaflow_protocol::DeploymentPayload::ReqStatus(_) => "DeploymentStatusRequest",
            tentaflow_protocol::DeploymentPayload::ResStatus(_) => "DeploymentStatusResponse",
            tentaflow_protocol::DeploymentPayload::ReqList(_) => "DeploymentListRequest",
            tentaflow_protocol::DeploymentPayload::ResList(_) => "DeploymentListResponse",
            tentaflow_protocol::DeploymentPayload::ReqLogStream(_) => "DeploymentLogStreamRequest",
            tentaflow_protocol::DeploymentPayload::StreamChunk(_) => "DeploymentStreamChunk",
            tentaflow_protocol::DeploymentPayload::StreamEnd(_) => "DeploymentStreamEnd",
        },
        MessageBody::ServiceBody(p) => match p {
            tentaflow_protocol::ServicePayload::ReqList(_) => "ServiceListRequest",
            tentaflow_protocol::ServicePayload::ResList(_) => "ServiceListResponse",
            tentaflow_protocol::ServicePayload::ReqDelete(_) => "ServiceDeleteRequest",
            tentaflow_protocol::ServicePayload::ResDelete(_) => "ServiceDeleteResponse",
            tentaflow_protocol::ServicePayload::ReqPin(_) => "ServicePinRequest",
            tentaflow_protocol::ServicePayload::ResPin(_) => "ServicePinResponse",
            tentaflow_protocol::ServicePayload::ReqPause(_) => "ServicePauseRequest",
            tentaflow_protocol::ServicePayload::ResPause(_) => "ServicePauseResponse",
            tentaflow_protocol::ServicePayload::ReqStart(_) => "ServiceStartRequest",
            tentaflow_protocol::ServicePayload::ResStart(_) => "ServiceStartResponse",
            tentaflow_protocol::ServicePayload::ReqUpdate(_) => "ServiceConfigUpdateRequest",
            tentaflow_protocol::ServicePayload::ResUpdate(_) => "ServiceConfigUpdateResponse",
            tentaflow_protocol::ServicePayload::ReqVramHint(_) => "ServiceVramHintRequest",
            tentaflow_protocol::ServicePayload::ResVramHint(_) => "ServiceVramHintResponse",
            tentaflow_protocol::ServicePayload::ReqEnginePresets(_) => {
                "ServiceEnginePresetsRequest"
            }
            tentaflow_protocol::ServicePayload::ResEnginePresets(_) => {
                "ServiceEnginePresetsResponse"
            }
        },
        MessageBody::SystemEventBody(p) => match p {
            tentaflow_protocol::SystemEventPayload::ServiceStatusChanged { .. } => {
                "ServiceStatusChanged"
            }
            tentaflow_protocol::SystemEventPayload::MeshPeerStatusChanged { .. } => {
                "MeshPeerStatusChanged"
            }
        },
        MessageBody::MeetingLiveEventBody(_) => "MeetingLiveEvent",
        MessageBody::MeetingBody(p) => match p {
            tentaflow_protocol::MeetingPayload::ReqSessionStart(_) => "MeetingSessionStartRequest",
            tentaflow_protocol::MeetingPayload::ResSessionStart(_) => "MeetingSessionStartResponse",
            tentaflow_protocol::MeetingPayload::ReqSessionLeave(_) => "MeetingSessionLeaveRequest",
            tentaflow_protocol::MeetingPayload::ResSessionLeave(_) => "MeetingSessionLeaveResponse",
            tentaflow_protocol::MeetingPayload::ReqSessionList(_) => "MeetingSessionListRequest",
            tentaflow_protocol::MeetingPayload::ResSessionList(_) => "MeetingSessionListResponse",
            tentaflow_protocol::MeetingPayload::ReqSessionDetail(_) => {
                "MeetingSessionDetailRequest"
            }
            tentaflow_protocol::MeetingPayload::ResSessionDetail(_) => {
                "MeetingSessionDetailResponse"
            }
            tentaflow_protocol::MeetingPayload::ReqTranscriptsList(_) => {
                "MeetingTranscriptsListRequest"
            }
            tentaflow_protocol::MeetingPayload::ResTranscriptsList(_) => {
                "MeetingTranscriptsListResponse"
            }
            tentaflow_protocol::MeetingPayload::ReqActiveSession(_) => {
                "MeetingActiveSessionRequest"
            }
            tentaflow_protocol::MeetingPayload::ResActiveSession(_) => {
                "MeetingActiveSessionResponse"
            }
            tentaflow_protocol::MeetingPayload::ReqSettingsGet(_) => "MeetingSettingsGetRequest",
            tentaflow_protocol::MeetingPayload::ResSettingsGet(_) => "MeetingSettingsGetResponse",
            tentaflow_protocol::MeetingPayload::ReqSettingsUpdate(_) => {
                "MeetingSettingsUpdateRequest"
            }
            tentaflow_protocol::MeetingPayload::ResSettingsUpdate(_) => {
                "MeetingSettingsUpdateResponse"
            }
            tentaflow_protocol::MeetingPayload::ReqSummariesList(_) => {
                "MeetingSummariesListRequest"
            }
            tentaflow_protocol::MeetingPayload::ResSummariesList(_) => {
                "MeetingSummariesListResponse"
            }
            tentaflow_protocol::MeetingPayload::ReqActionItemsList(_) => {
                "MeetingActionItemsListRequest"
            }
            tentaflow_protocol::MeetingPayload::ResActionItemsList(_) => {
                "MeetingActionItemsListResponse"
            }
            tentaflow_protocol::MeetingPayload::ReqActionItemStatusUpdate(_) => {
                "MeetingActionItemStatusUpdateRequest"
            }
            tentaflow_protocol::MeetingPayload::ResActionItemStatusUpdate(_) => {
                "MeetingActionItemStatusUpdateResponse"
            }
            tentaflow_protocol::MeetingPayload::ReqTranscriptExport(_) => {
                "MeetingTranscriptExportRequest"
            }
            tentaflow_protocol::MeetingPayload::ResTranscriptExport(_) => {
                "MeetingTranscriptExportResponse"
            }
            tentaflow_protocol::MeetingPayload::ReqWakeWord(_) => "MeetingWakeWordRequest",
            tentaflow_protocol::MeetingPayload::ResWakeWord(_) => "MeetingWakeWordResponse",
        },
        MessageBody::VncTunnelBody(p) => match p {
            tentaflow_protocol::VncTunnelPayload::ReqOpen(_) => "VncTunnelOpenRequest",
            tentaflow_protocol::VncTunnelPayload::ResOpen(_) => "VncTunnelOpenResponse",
            tentaflow_protocol::VncTunnelPayload::Chunk(_) => "VncTunnelChunk",
            tentaflow_protocol::VncTunnelPayload::ReqSend(_) => "VncTunnelSendRequest",
            tentaflow_protocol::VncTunnelPayload::ResSend(_) => "VncTunnelSendResponse",
            tentaflow_protocol::VncTunnelPayload::ReqClose(_) => "VncTunnelCloseRequest",
            tentaflow_protocol::VncTunnelPayload::ResClose(_) => "VncTunnelCloseResponse",
            tentaflow_protocol::VncTunnelPayload::StreamEnd(_) => "VncTunnelStreamEnd",
        },
        MessageBody::BrowserCaptureBody(payload) => match payload {
            tentaflow_protocol::BrowserCapturePayload::Request(_) => "BrowserCaptureRequest",
            tentaflow_protocol::BrowserCapturePayload::Response(_) => "BrowserCaptureResponse",
        },
        MessageBody::RegistryListRequest => "RegistryListRequest",
        MessageBody::RegistryListResponse { .. } => "RegistryListResponse",
        MessageBody::AuditEventBody(_) => "AuditEvent",
        MessageBody::ContainerBody(p) => match p {
            tentaflow_protocol::ContainerPayload::ListRequest => "ContainerListRequest",
            tentaflow_protocol::ContainerPayload::ListResponse { .. } => "ContainerListResponse",
            tentaflow_protocol::ContainerPayload::StartRequest { .. } => "ContainerStartRequest",
            tentaflow_protocol::ContainerPayload::StartResponse { .. } => "ContainerStartResponse",
            tentaflow_protocol::ContainerPayload::StopRequest { .. } => "ContainerStopRequest",
            tentaflow_protocol::ContainerPayload::StopResponse { .. } => "ContainerStopResponse",
            tentaflow_protocol::ContainerPayload::LogStreamRequest { .. } => {
                "ContainerLogStreamRequest"
            }
            tentaflow_protocol::ContainerPayload::LogChunkBody(_) => "ContainerLogChunk",
        },
        MessageBody::VoiceProfileListRequest => "VoiceProfileListRequest",
        MessageBody::VoiceProfileListResponse { .. } => "VoiceProfileListResponse",
        MessageBody::TtsRuleListRequest => "TtsRuleListRequest",
        MessageBody::TtsRuleListResponse { .. } => "TtsRuleListResponse",
        MessageBody::TtsRuleCreateRequest(_) => "TtsRuleCreateRequest",
        MessageBody::TtsRuleCreateResponse { .. } => "TtsRuleCreateResponse",
        MessageBody::TtsRuleDeleteRequest { .. } => "TtsRuleDeleteRequest",
        MessageBody::TtsRuleDeleteResponse { .. } => "TtsRuleDeleteResponse",
        MessageBody::PiiRuleBody(p) => match p {
            tentaflow_protocol::PiiRulePayload::ListRequest => "PiiRuleListRequest",
            tentaflow_protocol::PiiRulePayload::ListResponse { .. } => "PiiRuleListResponse",
        },
        MessageBody::VisionBody(p) => match p {
            tentaflow_protocol::VisionInferPayload::InferRequest(_) => "VisionInferRequest",
            tentaflow_protocol::VisionInferPayload::InferResponse(_) => "VisionInferResponse",
        },
        MessageBody::FastPathListRequest => "FastPathListRequest",
        MessageBody::FastPathListResponse { .. } => "FastPathListResponse",
        MessageBody::SettingsListRequest => "SettingsListRequest",
        MessageBody::SettingsListResponse { .. } => "SettingsListResponse",
        MessageBody::SettingsUpdateRequestBody(_) => "SettingsUpdateRequest",
        MessageBody::SettingsUpdateResponse { .. } => "SettingsUpdateResponse",
        MessageBody::NetworkBody(p) => match p {
            tentaflow_protocol::NetworkPayload::ReqInterfacesList => "NetworkInterfacesListRequest",
            tentaflow_protocol::NetworkPayload::ResInterfacesList { .. } => {
                "NetworkInterfacesListResponse"
            }
            tentaflow_protocol::NetworkPayload::ReqConfigGet => "NetworkConfigGetRequest",
            tentaflow_protocol::NetworkPayload::ResConfigGet(_) => "NetworkConfigGetResponse",
            tentaflow_protocol::NetworkPayload::ReqConfigUpdate(_) => "NetworkConfigUpdateRequest",
            tentaflow_protocol::NetworkPayload::ResConfigUpdate { .. } => {
                "NetworkConfigUpdateResponse"
            }
            tentaflow_protocol::NetworkPayload::ReqRelayStatus => "NetworkRelayStatusRequest",
            tentaflow_protocol::NetworkPayload::ResRelayStatus(_) => "NetworkRelayStatusResponse",
        },
        MessageBody::DashboardMetricsRequest => "DashboardMetricsRequest",
        MessageBody::DashboardMetricsResponse(_) => "DashboardMetricsResponse",
        MessageBody::MeshNodeListRequest => "MeshNodeListRequest",
        MessageBody::MeshNodeListResponseBody(_) => "MeshNodeListResponse",
        MessageBody::MeshNodeDetailRequestBody(_) => "MeshNodeDetailRequest",
        MessageBody::MeshNodeDetailResponseBody(_) => "MeshNodeDetailResponse",
        MessageBody::MeshPendingListRequest => "MeshPendingListRequest",
        MessageBody::MeshPendingListResponseBody(_) => "MeshPendingListResponse",
        MessageBody::MeshIdentityRequest => "MeshIdentityRequest",
        MessageBody::MeshIdentityResponseBody(_) => "MeshIdentityResponse",
        MessageBody::MeshServicesListRequest => "MeshServicesListRequest",
        MessageBody::MeshServicesListResponseBody(_) => "MeshServicesListResponse",
        MessageBody::MeshTrustedListRequest => "MeshTrustedListRequest",
        MessageBody::MeshTrustedListResponseBody(_) => "MeshTrustedListResponse",
        MessageBody::MeshPairingStartRequestBody(_) => "MeshPairingStartRequest",
        MessageBody::MeshPairingStartResponseBody(_) => "MeshPairingStartResponse",
        MessageBody::MeshPairingConfirmRequestBody(_) => "MeshPairingConfirmRequest",
        MessageBody::MeshPairingConfirmResponseBody(_) => "MeshPairingConfirmResponse",
        MessageBody::MeshPairingRejectRequestBody(_) => "MeshPairingRejectRequest",
        MessageBody::MeshPairingRejectResponseBody(_) => "MeshPairingRejectResponse",
        MessageBody::MeshTrustRevokeRequestBody(_) => "MeshTrustRevokeRequest",
        MessageBody::MeshTrustRevokeResponseBody(_) => "MeshTrustRevokeResponse",
        MessageBody::MeshTrustRetrustRequestBody(_) => "MeshTrustRetrustRequest",
        MessageBody::MeshTrustRetrustResponseBody(_) => "MeshTrustRetrustResponse",
        MessageBody::MeshConnectRequestBody(_) => "MeshConnectRequest",
        MessageBody::MeshConnectResponseBody(_) => "MeshConnectResponse",
        MessageBody::MeshNodeCommandRequestBody(_) => "MeshNodeCommandRequest",
        MessageBody::MeshNodeCommandResponseBody(_) => "MeshNodeCommandResponse",
        MessageBody::MeshNodeNetworkConfigRequestBody(_) => "MeshNodeNetworkConfigRequest",
        MessageBody::MeshNodeNetworkConfigResponseBody(_) => "MeshNodeNetworkConfigResponse",
        MessageBody::BaselineDonorListRequest => "BaselineDonorListRequest",
        MessageBody::BaselineDonorListResponseBody(_) => "BaselineDonorListResponse",
        MessageBody::BaselineAdoptStartRequestBody(_) => "BaselineAdoptStartRequest",
        MessageBody::BaselineAdoptStartResponseBody(_) => "BaselineAdoptStartResponse",
        MessageBody::BaselineAdoptStatusRequest => "BaselineAdoptStatusRequest",
        MessageBody::BaselineAdoptStatusResponseBody(_) => "BaselineAdoptStatusResponse",
        MessageBody::BaselineAdoptClearRequest => "BaselineAdoptClearRequest",
        MessageBody::BaselineAdoptClearResponseBody(_) => "BaselineAdoptClearResponse",
        MessageBody::CatalogListRequestBody(_) => "CatalogListRequest",
        MessageBody::CatalogListResponseBody(_) => "CatalogListResponse",
        MessageBody::ModelAliasListRequest => "ModelAliasListRequest",
        MessageBody::ModelAliasListResponseBody(_) => "ModelAliasListResponse",
        MessageBody::ModelAliasCreateRequestBody(_) => "ModelAliasCreateRequest",
        MessageBody::ModelAliasCreateResponseBody(_) => "ModelAliasCreateResponse",
        MessageBody::ModelAliasUpdateRequestBody(_) => "ModelAliasUpdateRequest",
        MessageBody::ModelAliasUpdateResponseBody(_) => "ModelAliasUpdateResponse",
        MessageBody::ModelAliasDeleteRequestBody(_) => "ModelAliasDeleteRequest",
        MessageBody::ModelAliasDeleteResponseBody(_) => "ModelAliasDeleteResponse",
        MessageBody::FlowUpdateRequestBody(_) => "FlowUpdateRequest",
        MessageBody::FlowUpdateResponseBody(_) => "FlowUpdateResponse",
        MessageBody::FlowNodeTemplatesListRequest => "FlowNodeTemplatesListRequest",
        MessageBody::FlowNodeTemplatesListResponseBody(_) => "FlowNodeTemplatesListResponse",
        MessageBody::FlowVersionListRequestBody(_) => "FlowVersionListRequest",
        MessageBody::FlowVersionListResponseBody(_) => "FlowVersionListResponse",
        MessageBody::FlowVersionGetRequestBody(_) => "FlowVersionGetRequest",
        MessageBody::FlowVersionGetResponseBody(_) => "FlowVersionGetResponse",
        MessageBody::FlowVersionRestoreRequestBody(_) => "FlowVersionRestoreRequest",
        MessageBody::FlowVersionRestoreResponseBody(_) => "FlowVersionRestoreResponse",
        MessageBody::SsoProvidersListRequest => "SsoProvidersListRequest",
        MessageBody::SsoProvidersListResponseBody(_) => "SsoProvidersListResponse",
        MessageBody::SsoProviderCreateRequestBody(_) => "SsoProviderCreateRequest",
        MessageBody::SsoProviderCreateResponseBody(_) => "SsoProviderCreateResponse",
        MessageBody::SsoProviderDeleteRequestBody(_) => "SsoProviderDeleteRequest",
        MessageBody::SsoProviderDeleteResponseBody(_) => "SsoProviderDeleteResponse",
        MessageBody::TlsStatusRequest => "TlsStatusRequest",
        MessageBody::TlsStatusResponseBody(_) => "TlsStatusResponse",
        MessageBody::NgcStatusRequest => "NgcStatusRequest",
        MessageBody::NgcStatusResponseBody(_) => "NgcStatusResponse",
        MessageBody::NimCatalogListRequest => "NimCatalogListRequest",
        MessageBody::NimCatalogListResponseBody(_) => "NimCatalogListResponse",
        MessageBody::DeployVllmRecommendRequestBody(_) => "DeployVllmRecommendRequest",
        MessageBody::DeployVllmRecommendResponseBody(_) => "DeployVllmRecommendResponse",
        MessageBody::SuggestServicePortRequestBody(_) => "SuggestServicePortRequest",
        MessageBody::SuggestServicePortResponseBody(_) => "SuggestServicePortResponse",
        MessageBody::EngineRecommendRequestBody(_) => "EngineRecommendRequest",
        MessageBody::EngineRecommendResponseBody(_) => "EngineRecommendResponse",
        // ServiceManifestDeploy przeniesione do DeploymentPayload::ReqStart/ResStart.
        MessageBody::AddonsListRequest => "AddonsListRequest",
        MessageBody::AddonsListResponseBody(_) => "AddonsListResponse",
        MessageBody::AddonUiBody(p) => match p {
            tentaflow_protocol::AddonUiPayload::ReqApplicationsList => {
                "AddonApplicationsListRequest"
            }
            tentaflow_protocol::AddonUiPayload::ResApplicationsList { .. } => {
                "AddonApplicationsListResponse"
            }
        },
        MessageBody::IamBody(p) => match p {
            tentaflow_protocol::IamPayload::ReqListUsers => "IamListUsersRequest",
            tentaflow_protocol::IamPayload::ResListUsers { .. } => "IamListUsersResponse",
            tentaflow_protocol::IamPayload::ReqGetUser { .. } => "IamGetUserRequest",
            tentaflow_protocol::IamPayload::ResGetUser { .. } => "IamGetUserResponse",
            tentaflow_protocol::IamPayload::ReqCreateUser { .. } => "IamCreateUserRequest",
            tentaflow_protocol::IamPayload::ResCreateUser { .. } => "IamCreateUserResponse",
            tentaflow_protocol::IamPayload::ReqUpdateUser { .. } => "IamUpdateUserRequest",
            tentaflow_protocol::IamPayload::ReqDeleteUser { .. } => "IamDeleteUserRequest",
            tentaflow_protocol::IamPayload::ReqSetUserGroups { .. } => "IamSetUserGroupsRequest",
            tentaflow_protocol::IamPayload::ReqResetUserPassword { .. } => {
                "IamResetUserPasswordRequest"
            }
            tentaflow_protocol::IamPayload::ReqListGroups => "IamListGroupsRequest",
            tentaflow_protocol::IamPayload::ResListGroups { .. } => "IamListGroupsResponse",
            tentaflow_protocol::IamPayload::ReqCreateGroup { .. } => "IamCreateGroupRequest",
            tentaflow_protocol::IamPayload::ResCreateGroup { .. } => "IamCreateGroupResponse",
            tentaflow_protocol::IamPayload::ReqUpdateGroup { .. } => "IamUpdateGroupRequest",
            tentaflow_protocol::IamPayload::ReqDeleteGroup { .. } => "IamDeleteGroupRequest",
            tentaflow_protocol::IamPayload::ReqGroupMembers { .. } => "IamGroupMembersRequest",
            tentaflow_protocol::IamPayload::ResGroupMembers { .. } => "IamGroupMembersResponse",
            tentaflow_protocol::IamPayload::ReqSetPermission { .. } => "IamSetPermissionRequest",
            tentaflow_protocol::IamPayload::ReqClearPermission { .. } => {
                "IamClearPermissionRequest"
            }
            tentaflow_protocol::IamPayload::ReqListPermsForResource { .. } => {
                "IamListPermsForResourceRequest"
            }
            tentaflow_protocol::IamPayload::ReqListPermsForSubject { .. } => {
                "IamListPermsForSubjectRequest"
            }
            tentaflow_protocol::IamPayload::ResListPermissions { .. } => {
                "IamListPermissionsResponse"
            }
            tentaflow_protocol::IamPayload::ResOk => "IamOkResponse",
        },
        MessageBody::AuditLogListRequestBody(_) => "AuditLogListRequest",
        MessageBody::AuditLogListResponseBody(_) => "AuditLogListResponse",
        MessageBody::AuditLogExportRequestBody(_) => "AuditLogExportRequest",
        MessageBody::AuditLogExportResponseBody(_) => "AuditLogExportResponse",
        MessageBody::AuditLogCleanupRequestBody(_) => "AuditLogCleanupRequest",
        MessageBody::AuditLogCleanupResponseBody(_) => "AuditLogCleanupResponse",
        MessageBody::AddonDetailRequestBody(_) => "AddonDetailRequest",
        MessageBody::AddonDetailResponseBody(_) => "AddonDetailResponse",
        MessageBody::AddonVisibilityListRequestBody(_) => "AddonVisibilityListRequest",
        MessageBody::AddonVisibilityListResponseBody(_) => "AddonVisibilityListResponse",
        MessageBody::AddonVisibilitySetRequestBody(_) => "AddonVisibilitySetRequest",
        MessageBody::AddonVisibilitySetResponseBody(_) => "AddonVisibilitySetResponse",
        MessageBody::AddonAdminOnlySetRequestBody(_) => "AddonAdminOnlySetRequest",
        MessageBody::AddonAdminOnlySetResponseBody(_) => "AddonAdminOnlySetResponse",
        MessageBody::AddonShowInCatalogSetRequestBody(_) => "AddonShowInCatalogSetRequest",
        MessageBody::AddonShowInCatalogSetResponseBody(_) => "AddonShowInCatalogSetResponse",
        MessageBody::AddonPermissionCatalogRequestBody(_) => "AddonPermissionCatalogRequest",
        MessageBody::AddonPermissionCatalogResponseBody(_) => "AddonPermissionCatalogResponse",
        MessageBody::AddonPermissionMatrixRequestBody(_) => "AddonPermissionMatrixRequest",
        MessageBody::AddonPermissionMatrixResponseBody(_) => "AddonPermissionMatrixResponse",
        MessageBody::AddonPermissionSetRequestBody(_) => "AddonPermissionSetRequest",
        MessageBody::AddonPermissionSetResponseBody(_) => "AddonPermissionSetResponse",
        MessageBody::AddonPermissionDefaultSetRequestBody(_) => "AddonPermissionDefaultSetRequest",
        MessageBody::AddonPermissionDefaultSetResponseBody(_) => {
            "AddonPermissionDefaultSetResponse"
        }
        MessageBody::AddonPermissionCheckRequestBody(_) => "AddonPermissionCheckRequest",
        MessageBody::AddonPermissionCheckResponseBody(_) => "AddonPermissionCheckResponse",
        MessageBody::AddonOAuthConfigListRequestBody(_) => "AddonOAuthConfigListRequest",
        MessageBody::AddonOAuthConfigListResponseBody(_) => "AddonOAuthConfigListResponse",
        MessageBody::AddonOAuthConfigSetRequestBody(_) => "AddonOAuthConfigSetRequest",
        MessageBody::AddonOAuthConfigSetResponseBody(_) => "AddonOAuthConfigSetResponse",
        MessageBody::AddonOAuthConfigClearSecretRequestBody(_) => {
            "AddonOAuthConfigClearSecretRequest"
        }
        MessageBody::AddonOAuthConfigClearSecretResponseBody(_) => {
            "AddonOAuthConfigClearSecretResponse"
        }
        MessageBody::AddonOAuthAuthorizeStartRequestBody(_) => "AddonOAuthAuthorizeStartRequest",
        MessageBody::AddonOAuthAuthorizeStartResponseBody(_) => "AddonOAuthAuthorizeStartResponse",
        MessageBody::AddonOAuthLinkedAccountsRequestBody(_) => "AddonOAuthLinkedAccountsRequest",
        MessageBody::AddonOAuthLinkedAccountsResponseBody(_) => "AddonOAuthLinkedAccountsResponse",
        MessageBody::AddonOAuthRevokeRequestBody(_) => "AddonOAuthRevokeRequest",
        MessageBody::AddonOAuthRevokeResponseBody(_) => "AddonOAuthRevokeResponse",
        MessageBody::AddonOAuthReauthorizeRequestBody(_) => "AddonOAuthReauthorizeRequest",
        MessageBody::AddonOAuthReauthorizeResponseBody(_) => "AddonOAuthReauthorizeResponse",
        MessageBody::AddonOAuthTestConnectionRequestBody(_) => "AddonOAuthTestConnectionRequest",
        MessageBody::AddonOAuthTestConnectionResponseBody(_) => "AddonOAuthTestConnectionResponse",
        MessageBody::MyOAuthAccountsListRequestBody(_) => "MyOAuthAccountsListRequest",
        MessageBody::MyOAuthAccountsListResponseBody(_) => "MyOAuthAccountsListResponse",
        MessageBody::AddonPermissionChangedEventBody(_) => "AddonPermissionChangedEvent",
        MessageBody::AddonToggleRequestBody(_) => "AddonToggleRequest",
        MessageBody::AddonToggleResponseBody(_) => "AddonToggleResponse",
        MessageBody::AddonInstallRequestBody(_) => "AddonInstallRequest",
        MessageBody::AddonInstallResponseBody(_) => "AddonInstallResponse",
        MessageBody::AddonUninstallRequestBody(_) => "AddonUninstallRequest",
        MessageBody::AddonUninstallResponseBody(_) => "AddonUninstallResponse",
        MessageBody::AddonConfigGetRequestBody(_) => "AddonConfigGetRequest",
        MessageBody::AddonConfigGetResponseBody(_) => "AddonConfigGetResponse",
        MessageBody::AddonConfigSetRequestBody(_) => "AddonConfigSetRequest",
        MessageBody::AddonConfigSetResponseBody(_) => "AddonConfigSetResponse",
        MessageBody::AddonLogsRequestBody(_) => "AddonLogsRequest",
        MessageBody::AddonLogsResponseBody(_) => "AddonLogsResponse",
        MessageBody::AddonToolsRequestBody(_) => "AddonToolsRequest",
        MessageBody::AddonToolsResponseBody(_) => "AddonToolsResponse",
        MessageBody::AddonResourcesGetRequestBody(_) => "AddonResourcesGetRequest",
        MessageBody::AddonResourcesGetResponseBody(_) => "AddonResourcesGetResponse",
        MessageBody::AddonResourcesSetRequestBody(_) => "AddonResourcesSetRequest",
        MessageBody::AddonResourcesSetResponseBody(_) => "AddonResourcesSetResponse",
        MessageBody::AddonNetworkRulesGetRequestBody(_) => "AddonNetworkRulesGetRequest",
        MessageBody::AddonNetworkRulesGetResponseBody(_) => "AddonNetworkRulesGetResponse",
        MessageBody::AddonNetworkRulesSetRequestBody(_) => "AddonNetworkRulesSetRequest",
        MessageBody::AddonNetworkRulesSetResponseBody(_) => "AddonNetworkRulesSetResponse",
        MessageBody::AddonReloadRequestBody(_) => "AddonReloadRequest",
        MessageBody::AddonReloadResponseBody(_) => "AddonReloadResponse",
        MessageBody::UiChannelCbor(_) => "UiChannelCbor",
        MessageBody::RoleCatalogBody(p) => match p {
            tentaflow_protocol::RoleCatalogPayload::ListRequest(_) => "RoleCatalogListRequest",
            tentaflow_protocol::RoleCatalogPayload::ListResponse { .. } => {
                "RoleCatalogListResponse"
            }
            tentaflow_protocol::RoleCatalogPayload::GetRequest { .. } => "RoleCatalogGetRequest",
            tentaflow_protocol::RoleCatalogPayload::GetBySlugRequest { .. } => {
                "RoleCatalogGetBySlugRequest"
            }
            tentaflow_protocol::RoleCatalogPayload::GetResponse { .. } => "RoleCatalogGetResponse",
            tentaflow_protocol::RoleCatalogPayload::ListLocalesRequest => {
                "RoleCatalogListLocalesRequest"
            }
            tentaflow_protocol::RoleCatalogPayload::ListLocalesResponse { .. } => {
                "RoleCatalogListLocalesResponse"
            }
            tentaflow_protocol::RoleCatalogPayload::CreateRequest(_) => "RoleCatalogCreateRequest",
            tentaflow_protocol::RoleCatalogPayload::CreateResponse(_) => {
                "RoleCatalogCreateResponse"
            }
            tentaflow_protocol::RoleCatalogPayload::UpdateRequest(_) => "RoleCatalogUpdateRequest",
            tentaflow_protocol::RoleCatalogPayload::UpdateResponse(_) => {
                "RoleCatalogUpdateResponse"
            }
            tentaflow_protocol::RoleCatalogPayload::DeactivateRequest { .. } => {
                "RoleCatalogDeactivateRequest"
            }
            tentaflow_protocol::RoleCatalogPayload::DeactivateResponse { .. } => {
                "RoleCatalogDeactivateResponse"
            }
        },
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
        assert!(
            SessionAuthKind::Anonymous.session_satisfies(&SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: None,
            })
        );
    }

    #[test]
    fn session_auth_kind_admin_requires_role_admin() {
        let kind = SessionAuthKind::Admin;
        // UserSession z role=admin → OK
        assert!(kind.session_satisfies(&SessionAuth::UserSession {
            user_id: [0u8; 16],
            role: Some("admin".to_string()),
        }));
        // UserSession z innym role → reject
        assert!(!kind.session_satisfies(&SessionAuth::UserSession {
            user_id: [0u8; 16],
            role: Some("user".to_string()),
        }));
        // UserSession bez role → reject (Zero Trust default)
        assert!(!kind.session_satisfies(&SessionAuth::UserSession {
            user_id: [0u8; 16],
            role: None,
        }));
        // Inne sesje → reject
        assert!(!kind.session_satisfies(&SessionAuth::Anonymous));
        assert!(!kind.session_satisfies(&SessionAuth::ApiKey {
            key_id: "x".to_string()
        }));
    }

    #[test]
    fn session_auth_kind_user_session_requires_exact_match() {
        let kind = SessionAuthKind::UserSession;
        assert!(kind.session_satisfies(&SessionAuth::UserSession {
            user_id: [0u8; 16],
            role: None
        }));
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
        assert!(kind.session_satisfies(&SessionAuth::UserSession {
            user_id: [0u8; 16],
            role: None
        }));
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
        assert!(!kind.session_satisfies(&SessionAuth::UserSession {
            user_id: [0u8; 16],
            role: None
        }));
    }

    #[tokio::test]
    async fn dispatch_unknown_variant_returns_not_implemented() {
        // Variants are all known by variant_name_of, ale jesli handler nie
        // zarejestrowany w registry (np. Error) zwraca NotImplemented.
        // NOTE: ten test zalezy od tego ze handler dla Error nie istnieje
        // (bo Error to output variant, nie input).
        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: state::AppState::for_test(),
            org_context: None,
        };
        let body = MessageBody::Error(ProtocolError {
            code: ProtocolErrorCode::Internal,
            message: "test".to_string(),
            trace_id: None,
        });
        let (resp, is_err) = dispatch(&body, &ctx).await;
        assert!(is_err);
        match resp {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::NotImplemented),
            _ => panic!("expected error"),
        }
    }

    fn sync_test_ctx(role: &str) -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [1u8; 16],
                role: Some(role.to_string()),
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: state::AppState::for_test(),
            org_context: None,
        }
    }

    fn with_tmp_home<F: FnOnce()>(f: F) {
        let _guard = crate::addon::fs_sandbox::test_home_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        f();
        if let Some(p) = prev {
            std::env::set_var("HOME", p);
        } else {
            std::env::remove_var("HOME");
        }
    }

    fn sync_conflict_capture(addon_id: &str) -> crate::sync::runtime::SqlWriteCapture {
        crate::sync::runtime::SqlWriteCapture {
            capture_id: format!("dispatch-conflict-{addon_id}"),
            org_id: "org-default".to_string(),
            addon_id: addon_id.to_string(),
            table_name: "contacts".to_string(),
            action: crate::sync::runtime::SqlWriteAction::Insert,
            resource_type: "contacts".to_string(),
            resource_id: "1".to_string(),
            query: "INSERT INTO contacts (id, name) VALUES (?1, ?2)".to_string(),
            params: vec![
                serde_json::Value::from(1),
                serde_json::Value::String("Ewa".to_string()),
            ],
            rows_affected: 1,
            last_insert_id: 1,
            actor_user_id: Some("1".to_string()),
            created_at_ms: crate::sync::runtime::now_ms(),
        }
    }

    #[test]
    fn sync_conflict_list_dispatch_returns_rows_for_admin() {
        with_tmp_home(|| {
            let addon_id = "dispatch-conflict-list-test";
            let operation_id = crate::sync::ledger::OperationId::from_hash([0xA1; 32]);
            crate::addon::storage_sql_exec::record_sync_conflict(
                &sync_conflict_capture(addon_id),
                operation_id,
                "node-b",
                &crate::addon::storage_sql_exec::StorageSqlError::SqlConstraint,
            )
            .expect("record conflict");

            let ctx = sync_test_ctx("admin");
            let body = MessageBody::SyncConflictBody(
                tentaflow_protocol::SyncConflictPayload::ListRequest(
                    tentaflow_protocol::SyncConflictsListRequest {
                        org_id: "org-default".to_string(),
                        addon_id: addon_id.to_string(),
                        status: "open".to_string(),
                        limit: 10,
                    },
                ),
            );
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            let (resp, is_err) = runtime.block_on(dispatch(&body, &ctx));

            assert!(!is_err);
            match resp {
                MessageBody::SyncConflictBody(
                    tentaflow_protocol::SyncConflictPayload::ListResponse(response),
                ) => {
                    assert_eq!(response.conflicts.len(), 1);
                    assert_eq!(response.conflicts[0].operation_id, operation_id.to_hex());
                }
                _ => panic!("expected sync conflict list response"),
            }
        });
    }

    #[tokio::test]
    async fn sync_conflict_list_dispatch_rejects_non_admin() {
        let ctx = sync_test_ctx("user");
        let body =
            MessageBody::SyncConflictBody(tentaflow_protocol::SyncConflictPayload::ListRequest(
                tentaflow_protocol::SyncConflictsListRequest {
                    org_id: "org-default".to_string(),
                    addon_id: "contacts".to_string(),
                    status: "open".to_string(),
                    limit: 10,
                },
            ));

        let (resp, is_err) = dispatch(&body, &ctx).await;

        assert!(is_err);
        match resp {
            MessageBody::Error(error) => assert_eq!(error.code, ProtocolErrorCode::PolicyDenied),
            _ => panic!("expected auth error"),
        }
    }

    #[tokio::test]
    async fn sync_conflict_resolve_dispatch_rejects_non_admin() {
        let ctx = sync_test_ctx("user");
        let body =
            MessageBody::SyncConflictBody(tentaflow_protocol::SyncConflictPayload::ResolveRequest(
                tentaflow_protocol::SyncConflictResolveRequest {
                    org_id: "org-default".to_string(),
                    addon_id: "contacts".to_string(),
                    operation_id: crate::sync::ledger::OperationId::from_hash([0xC3; 32]).to_hex(),
                    resolution: tentaflow_protocol::SyncConflictResolution::KeepLocal,
                },
            ));

        let (resp, is_err) = dispatch(&body, &ctx).await;

        assert!(is_err);
        match resp {
            MessageBody::Error(error) => assert_eq!(error.code, ProtocolErrorCode::PolicyDenied),
            _ => panic!("expected auth error"),
        }
    }

    #[tokio::test]
    async fn sync_conflict_list_dispatch_rejects_invalid_scope_and_status() {
        let ctx = sync_test_ctx("admin");
        for (addon_id, status) in [("../contacts", "open"), ("contacts", "everything")] {
            let body = MessageBody::SyncConflictBody(
                tentaflow_protocol::SyncConflictPayload::ListRequest(
                    tentaflow_protocol::SyncConflictsListRequest {
                        org_id: "org-default".to_string(),
                        addon_id: addon_id.to_string(),
                        status: status.to_string(),
                        limit: 10,
                    },
                ),
            );
            let (resp, is_err) = dispatch(&body, &ctx).await;
            assert!(is_err);
            match resp {
                MessageBody::Error(error) => assert_eq!(error.code, ProtocolErrorCode::BadRequest),
                _ => panic!("expected bad request"),
            }
        }
    }

    #[tokio::test]
    async fn sync_conflict_resolve_dispatch_rejects_invalid_operation_id() {
        let ctx = sync_test_ctx("admin");
        let body =
            MessageBody::SyncConflictBody(tentaflow_protocol::SyncConflictPayload::ResolveRequest(
                tentaflow_protocol::SyncConflictResolveRequest {
                    org_id: "org-default".to_string(),
                    addon_id: "contacts".to_string(),
                    operation_id: "not-hex".to_string(),
                    resolution: tentaflow_protocol::SyncConflictResolution::AcceptRemote,
                },
            ));

        let (resp, is_err) = dispatch(&body, &ctx).await;

        assert!(is_err);
        match resp {
            MessageBody::Error(error) => assert_eq!(error.code, ProtocolErrorCode::BadRequest),
            _ => panic!("expected bad request"),
        }
    }

    #[test]
    fn sync_conflict_resolve_dispatch_marks_conflict_for_admin() {
        with_tmp_home(|| {
            let addon_id = "dispatch-conflict-resolve-test";
            let operation_id = crate::sync::ledger::OperationId::from_hash([0xB2; 32]);
            let db_path = crate::paths::tentaflow_home().join("dispatch-sync-runtime.db");
            let db = crate::db::init(&db_path).expect("test DB init");
            let cipher = std::sync::Arc::new(crate::crypto::SettingsCipher::new(&[0xB2; 32]));
            let security = std::sync::Arc::new(
                crate::mesh::security::MeshSecurity::new(db, cipher.clone())
                    .expect("mesh security"),
            );
            crate::sync::runtime::init(security.db.clone(), security, cipher)
                .expect("sync runtime");
            crate::addon::storage_sql_exec::record_sync_conflict(
                &sync_conflict_capture(addon_id),
                operation_id,
                "node-b",
                &crate::addon::storage_sql_exec::StorageSqlError::SqlConstraint,
            )
            .expect("record conflict");

            let ctx = sync_test_ctx("admin");
            let body = MessageBody::SyncConflictBody(
                tentaflow_protocol::SyncConflictPayload::ResolveRequest(
                    tentaflow_protocol::SyncConflictResolveRequest {
                        org_id: "org-default".to_string(),
                        addon_id: addon_id.to_string(),
                        operation_id: operation_id.to_hex(),
                        resolution: tentaflow_protocol::SyncConflictResolution::KeepLocal,
                    },
                ),
            );
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            let (resp, is_err) = runtime.block_on(dispatch(&body, &ctx));

            assert!(!is_err);
            match resp {
                MessageBody::SyncConflictBody(
                    tentaflow_protocol::SyncConflictPayload::ResolveResponse(response),
                ) => {
                    assert_eq!(response.operation_id, operation_id.to_hex());
                    assert_eq!(response.status, "ignored");
                    assert_eq!(response.resolution, "keep_local");
                }
                _ => panic!("expected sync conflict resolve response"),
            }
            let conflicts = crate::addon::storage_sql_exec::list_sync_conflicts(
                "org-default",
                addon_id,
                Some("ignored"),
                10,
            )
            .expect("resolved conflicts");
            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].operation_id, operation_id.to_hex());
        });
    }

    #[test]
    fn sync_storage_report_dispatch_returns_report_for_admin() {
        with_tmp_home(|| {
            let home = crate::paths::tentaflow_home();
            std::fs::create_dir_all(home.join("data")).expect("data dir");
            std::fs::write(home.join("data/router.db"), vec![1u8; 9]).expect("router db");

            let ctx = sync_test_ctx("admin");
            let body = MessageBody::SyncStorageBody(
                tentaflow_protocol::SyncStoragePayload::ReportRequest(
                    tentaflow_protocol::SyncStorageReportRequest,
                ),
            );
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            let (resp, is_err) = runtime.block_on(dispatch(&body, &ctx));

            assert!(!is_err);
            match resp {
                MessageBody::SyncStorageBody(
                    tentaflow_protocol::SyncStoragePayload::ReportResponse(response),
                ) => {
                    assert_eq!(response.sqlite_bytes, 9);
                    assert_eq!(
                        response.large_blob_block_bytes,
                        crate::sync::storage_monitor::LARGE_BLOB_BLOCK_BYTES
                    );
                    assert!(response.paths.iter().any(|path| path.label == "sqlite"));
                }
                _ => panic!("expected sync storage report response"),
            }
        });
    }

    #[tokio::test]
    async fn sync_storage_report_dispatch_rejects_non_admin() {
        let ctx = sync_test_ctx("user");
        let body =
            MessageBody::SyncStorageBody(tentaflow_protocol::SyncStoragePayload::ReportRequest(
                tentaflow_protocol::SyncStorageReportRequest,
            ));

        let (resp, is_err) = dispatch(&body, &ctx).await;

        assert!(is_err);
        match resp {
            MessageBody::Error(error) => assert_eq!(error.code, ProtocolErrorCode::PolicyDenied),
            _ => panic!("expected auth error"),
        }
    }

    #[test]
    fn registry_contains_addon_lifecycle_handlers() {
        // 12 handlerow lifecycle addonu musi byc zarejestrowane przez inventory.
        for name in [
            "AddonToggleRequest",
            "AddonInstallRequest",
            "AddonUninstallRequest",
            "AddonConfigGetRequest",
            "AddonConfigSetRequest",
            "AddonLogsRequest",
            "AddonToolsRequest",
            "AddonResourcesGetRequest",
            "AddonResourcesSetRequest",
            "AddonNetworkRulesGetRequest",
            "AddonNetworkRulesSetRequest",
            "AddonReloadRequest",
        ] {
            assert!(find(name).is_some(), "handler {} nie zarejestrowany", name);
        }
    }

    // Bug guard: wszystkie 8 meeting handlers (api/dashboard/handlers_meeting.rs)
    // musi byc widocznych w dispatch registry — inaczej GUI dostaje HandlerNotFound
    // na WSS i ekran meetingow nie dziala. Ten test lapie regresje gdyby ktos
    // przypadkiem wylaczyl feature/module.
    #[test]
    fn registry_contains_meeting_handlers() {
        for name in [
            "MeetingSessionStartRequest",
            "MeetingSessionLeaveRequest",
            "MeetingSessionListRequest",
            "MeetingSessionDetailRequest",
            "MeetingTranscriptsListRequest",
            "MeetingActiveSessionRequest",
            "MeetingSettingsGetRequest",
            "MeetingSettingsUpdateRequest",
            "MeetingSummariesListRequest",
            "MeetingActionItemsListRequest",
            "MeetingActionItemStatusUpdateRequest",
            "MeetingTranscriptExportRequest",
        ] {
            assert!(
                find(name).is_some(),
                "meeting handler {} nie zarejestrowany",
                name
            );
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
        assert!(find("ModelListRequest").is_some());
        assert!(find("ApiKeyListRequest").is_some());
        assert!(find("AuthLoginRequest").is_some());
        // ChatStreamRequest is registered as STREAMING handler (stream_handlers.rs),
        // not in sync registry — verify via streaming registry instead.
        assert!(subscription::find_stream_handler("ChatStreamRequest").is_some());
        assert!(find("ClusterUpdateRequest").is_some());
    }

    #[tokio::test]
    async fn dispatch_archetype_coverage_real_handlers() {
        use tentaflow_protocol::{AuthLoginRequest, ClusterUpdateRequest};

        // Session user_id = 16 surowych bajtow UUID (raw wire form).
        let user_bytes = *uuid::Uuid::nil().as_bytes();

        let ctx_user = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: user_bytes,
                role: None,
            },
            correlation_id: 100,
            connection_id: 0,
            resume_secret: None,
            state: state::AppState::for_test(),
            org_context: None,
        };

        // R-LIST — empty test DB → empty Vec, valid response.
        let r_list = dispatch(&MessageBody::ApiKeyListRequest, &ctx_user).await;
        assert!(!r_list.1);
        assert!(matches!(r_list.0, MessageBody::ApiKeyListResponse { .. }));

        let model_list = dispatch(&MessageBody::ModelListRequest, &ctx_user).await;
        assert!(!model_list.1);
        assert!(matches!(
            model_list.0,
            MessageBody::ModelListResponse { .. }
        ));

        let flow_list = dispatch(&MessageBody::FlowListRequest, &ctx_user).await;
        assert!(!flow_list.1);
        assert!(matches!(flow_list.0, MessageBody::FlowListResponse { .. }));

        // W-ACTION login z fake credentials → AuthRequired (real auth check).
        let w_action = dispatch(
            &MessageBody::AuthLoginRequestBody(AuthLoginRequest {
                username: "nonexistent".to_string(),
                password: "wrong".to_string(),
            }),
            &HandlerContext {
                session: SessionAuth::Anonymous,
                correlation_id: 1,
                connection_id: 0,
                resume_secret: None,
                state: state::AppState::for_test(),
                org_context: None,
            },
        )
        .await;
        assert!(w_action.1);
        match w_action.0 {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::AuthRequired),
            other => panic!("expected AuthRequired, got {:?}", other),
        }

        // Admin-only handler bez admin role → PolicyDenied.
        let w_update_no_admin = dispatch(
            &MessageBody::ClusterUpdateRequestBody(ClusterUpdateRequest {
                cluster_id: "c1".to_string(),
                name: Some("Prod".to_string()),
                description: None,
                strategy: None,
                failover_enabled: None,
                failover_target: None,
                health_check_interval_ms: None,
                timeout_ms: None,
            }),
            &ctx_user,
        )
        .await;
        assert!(w_update_no_admin.1);
        match w_update_no_admin.0 {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::PolicyDenied),
            other => panic!("expected PolicyDenied, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn dispatch_api_key_list_request_via_registry() {
        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: None,
            },
            correlation_id: 7,
            connection_id: 0,
            resume_secret: None,
            state: state::AppState::for_test(),
            org_context: None,
        };
        let (resp, is_err) = dispatch(&MessageBody::ApiKeyListRequest, &ctx).await;
        assert!(!is_err);
        assert!(matches!(resp, MessageBody::ApiKeyListResponse { .. }));
    }

    #[tokio::test]
    async fn dispatch_policy_denies_anonymous_for_api_key_list() {
        let ctx = HandlerContext {
            session: SessionAuth::Anonymous,
            correlation_id: 8,
            connection_id: 0,
            resume_secret: None,
            state: state::AppState::for_test(),
            org_context: None,
        };
        let (resp, is_err) = dispatch(&MessageBody::ApiKeyListRequest, &ctx).await;
        assert!(is_err);
        match resp {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::PolicyDenied),
            _ => panic!("expected PolicyDenied error"),
        }
    }

    #[tokio::test]
    async fn dispatch_model_list_allows_anonymous() {
        let ctx = HandlerContext {
            session: SessionAuth::Anonymous,
            correlation_id: 9,
            connection_id: 0,
            resume_secret: None,
            state: state::AppState::for_test(),
            org_context: None,
        };
        let (resp, is_err) = dispatch(&MessageBody::ModelListRequest, &ctx).await;
        assert!(!is_err);
        assert!(matches!(resp, MessageBody::ModelListResponse { .. }));
    }
}

// =============================================================================
// Testy enforcementu widocznosci addonow (list + detail + check + tools)
// =============================================================================

#[cfg(test)]
mod visibility_enforcement_tests {
    use super::*;
    use crate::db::repository;

    /// Helper: koduje UUID user_id do 16 surowych bajtow sesji.
    fn user_id_bytes(id: &str) -> [u8; 16] {
        *uuid::Uuid::parse_str(id).expect("valid uuid").as_bytes()
    }

    /// Helper: tworzy testowy user i rejestruje addon w DB. Zwraca user_id (UUID).
    fn setup_user_and_addon(db: &crate::db::DbPool, username: &str, addon_id: &str) -> String {
        repository::register_addon(db, addon_id, addon_id, "1.0.0", "{}", "linux")
            .expect("register_addon failed");
        repository::create_user_account(db, username, "hash", username, "a@a.pl")
            .expect("create_user failed")
    }

    #[tokio::test]
    async fn test_addons_list_filters_admin_only_for_non_admin() {
        let state = state::AppState::for_test();
        // Dwa addony: jeden admin_only, drugi zwykly.
        repository::register_addon(&state.db, "public-addon", "Public", "1.0.0", "{}", "linux")
            .unwrap();
        repository::register_addon(&state.db, "secret-addon", "Secret", "1.0.0", "{}", "linux")
            .unwrap();
        repository::set_addon_admin_only(&state.db, "secret-addon", true).unwrap();
        let user_id =
            repository::create_user_account(&state.db, "john", "h", "john", "j@j.pl").unwrap();

        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: user_id_bytes(&user_id),
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            org_context: None,
        };

        let (resp, is_err) = dispatch(&MessageBody::AddonsListRequest, &ctx).await;
        assert!(!is_err);
        match resp {
            MessageBody::AddonsListResponseBody(r) => {
                let ids: Vec<_> = r.addons.iter().map(|a| a.addon_id.as_str()).collect();
                assert!(ids.contains(&"public-addon"));
                assert!(
                    !ids.contains(&"secret-addon"),
                    "non-admin nie powinien widziec admin_only"
                );
            }
            other => panic!("unexpected response: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_addons_list_filters_by_group_visibility() {
        let state = state::AppState::for_test();
        repository::register_addon(&state.db, "grp-addon", "Grp", "1.0.0", "{}", "linux").unwrap();
        let user_id =
            repository::create_user_account(&state.db, "anna", "h", "anna", "a@x.pl").unwrap();
        // Grupa B — user tam NIE nalezy; addon widoczny tylko dla grupy B.
        let gb = repository::create_group(&state.db, "groupB", "").unwrap();
        repository::set_addon_visibility(&state.db, "grp-addon", &gb, true, None).unwrap();

        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: user_id_bytes(&user_id),
                role: None,
            },
            correlation_id: 2,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            org_context: None,
        };

        let (resp, _) = dispatch(&MessageBody::AddonsListRequest, &ctx).await;
        match resp {
            MessageBody::AddonsListResponseBody(r) => {
                assert!(r.addons.iter().all(|a| a.addon_id != "grp-addon"));
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_addons_list_admin_sees_all() {
        let state = state::AppState::for_test();
        repository::register_addon(&state.db, "a1", "A1", "1.0.0", "{}", "linux").unwrap();
        repository::register_addon(&state.db, "a2", "A2", "1.0.0", "{}", "linux").unwrap();
        repository::set_addon_admin_only(&state.db, "a2", true).unwrap();
        let user_id =
            repository::create_user_account(&state.db, "root", "h", "root", "r@r.pl").unwrap();

        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: user_id_bytes(&user_id),
                role: Some("admin".to_string()),
            },
            correlation_id: 3,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            org_context: None,
        };

        let (resp, _) = dispatch(&MessageBody::AddonsListRequest, &ctx).await;
        match resp {
            MessageBody::AddonsListResponseBody(r) => {
                assert_eq!(r.addons.len(), 2, "admin powinien widziec wszystkie addony");
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_addon_detail_returns_not_found_for_hidden() {
        let state = state::AppState::for_test();
        let user_id = setup_user_and_addon(&state.db, "bob", "hidden");
        repository::set_addon_admin_only(&state.db, "hidden", true).unwrap();

        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: user_id_bytes(&user_id),
                role: None,
            },
            correlation_id: 4,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            org_context: None,
        };

        let (resp, is_err) = dispatch(
            &MessageBody::AddonDetailRequestBody(tentaflow_protocol::AddonDetailRequest {
                addon_id: "hidden".to_string(),
            }),
            &ctx,
        )
        .await;
        assert!(is_err);
        match resp {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::NotFound),
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_addon_permission_check_returns_not_found_for_hidden() {
        let state = state::AppState::for_test();
        let user_id = setup_user_and_addon(&state.db, "carol", "hidden2");
        repository::set_addon_admin_only(&state.db, "hidden2", true).unwrap();

        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: user_id_bytes(&user_id),
                role: None,
            },
            correlation_id: 5,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            org_context: None,
        };

        let (resp, is_err) = dispatch(
            &MessageBody::AddonPermissionCheckRequestBody(
                tentaflow_protocol::AddonPermissionCheckRequest {
                    addon_id: "hidden2".to_string(),
                    permission_id: "some.perm".to_string(),
                    user_id: None,
                },
            ),
            &ctx,
        )
        .await;
        assert!(is_err);
        match resp {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::NotFound),
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_addon_tools_returns_not_found_for_hidden() {
        let state = state::AppState::for_test();
        let user_id = setup_user_and_addon(&state.db, "dave", "hidden3");
        repository::set_addon_admin_only(&state.db, "hidden3", true).unwrap();

        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: user_id_bytes(&user_id),
                role: None,
            },
            correlation_id: 6,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            org_context: None,
        };

        let (resp, is_err) = dispatch(
            &MessageBody::AddonToolsRequestBody(tentaflow_protocol::AddonToolsRequest {
                addon_id: "hidden3".to_string(),
            }),
            &ctx,
        )
        .await;
        assert!(is_err);
        match resp {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::NotFound),
            other => panic!("expected NotFound, got {:?}", other),
        }
    }
}
