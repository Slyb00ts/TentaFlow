// ============================================================================
// TENTAFLOW PROTOCOL - Wspólne typy dla Router ↔ RAG
// ============================================================================
//
// CEL:
// Definicje typów protokołu QUIC + rkyv używanych w komunikacji między
// TentaFlow.Router a TentaFlow.RAG. Typy są serializowane używając rkyv
// (zero-copy) dla maksymalnej wydajności.
//
// KLUCZOWE KONCEPCJE:
// - rkyv: Zero-copy deserialization (10x szybsze niż serde)
// - Archive: Trait dla archived representation (zero-copy access)
// - Serialize/Deserialize: Traits dla konwersji to/from archived form
// - #[archive(check_bytes)]: Runtime validation dla bezpieczeństwa
//
// UWAGI:
// - Wszystkie typy muszą implementować Archive + Serialize + Deserialize
// - Używamy #[archive(check_bytes)] dla walidacji (ochrona przed corrupted data)
// - Stringi są serializowane jako archived strings (zero-copy)
//
// ============================================================================

pub mod types;
pub mod mesh;
pub mod envelope;
pub mod message_body;

pub use types::*;
pub use mesh::*;
pub use envelope::{
    message_kind, Envelope, EnvelopeFlags, Routing, SessionAuth, SignedSessionClaim,
    SCHEMA_VERSION,
};
pub use message_body::{
    ApiKeyCreateRequest, ApiKeyCreateResponse, ApiKeySummary, AuditEvent, AuditLogCleanupRequest,
    AuditLogCleanupResponse, AuditLogEntry, AuditLogExportRequest, AuditLogExportResponse,
    AuditLogFilters, AuditLogListRequest, AuditLogListResponse, AuthLoginRequest,
    AuthLoginResponse, AuthMeResponse, ChatMessage, ChatStreamChunk, ChatStreamEnd,
    ChatStreamRequest, ClusterAddMemberRequest, ClusterAddMemberResponse, ClusterCreateRequest,
    ClusterCreateResponse, ClusterDeleteRequest, ClusterDeleteResponse, ClusterDetailRequest,
    ClusterDetailResponse, ClusterInfo, ClusterListResponse, ClusterMember,
    ClusterProbeStreamChunk, ClusterProbeStreamEnd, ClusterProbeStreamRequest,
    ClusterRemoveMemberRequest, ClusterRemoveMemberResponse, ClusterUpdateRequest,
    ClusterUpdateResponse, ContainerLogChunk, ContainerSummary, DashboardSnapshot,
    FastPathPattern, FlowCreateRequest, FlowDetail, FlowExecutionSummary,
    FlowNodeTemplate, FlowNodeTemplatesListResponse, FlowSummary, FlowUpdateRequest,
    FlowUpdateResponse, FlowVersionFull, FlowVersionGetRequest, FlowVersionGetResponse,
    FlowVersionListRequest, FlowVersionListResponse, FlowVersionRestoreRequest,
    FlowVersionRestoreResponse, FlowVersionSummary,
    HubDownloadProgress, HubEngineSummary, HubModelSearchResult, MeshConnectRequest,
    MeshConnectResponse, MeshIdentityResponse, MeshNodeCommandRequest, MeshNodeCommandResponse,
    MeshNodeContainer, MeshNodeDetailRequest, MeshNodeDetailResponse, MeshNodeGpuInfo,
    MeshNodeInfo, MeshNodeListResponse, MeshNodeModel, MeshNodeNetworkConfigRequest,
    MeshNodeNetworkConfigResponse, MeshNodeNetworkInterface, MeshNodeRoute, MeshPairInitRequest,
    MeshPairInitResponse, MeshPairingConfirmRequest, MeshPairingConfirmResponse,
    MeshPairingRejectRequest, MeshPairingRejectResponse, MeshPairingStartRequest,
    MeshPairingStartResponse, MeshPeerSummary, MeshPendingListResponse, MeshPendingPair,
    MeshServicesEntry, MeshServicesListResponse, MeshTrustRetrustRequest,
    MeshTrustRetrustResponse, MeshTrustRevokeRequest, MeshTrustRevokeResponse,
    MeshTrustRevokedEvent, MeshTrustedKeysSyncEvent, MeshTrustedListResponse, MeshTrustedNode,
    MessageBody, ModelDetail, ModelInstallRequest, ModelSummary, NodeSummary, PiiRule,
    PromptDetail, PromptSummary, ProtocolError, ProtocolErrorCode, RegistrySummary,
    ServiceCreateRequest, ServiceDeployProgress, ServiceDeployRequest, ServiceQuicStatus,
    ServiceSummary, ServiceUpdateRequest, SettingEntry, SettingsUpdateRequest,
    SsoProviderCreateRequest, SsoProviderCreateResponse, SsoProviderDeleteRequest,
    SsoProviderDeleteResponse, SsoProviderEntry, SsoProvidersListResponse,
    TlsStatusResponse, NgcStatusResponse, TtsRule, VoiceProfileSummary,
    ModelAliasCreateRequest, ModelAliasCreateResponse, ModelAliasDeleteRequest,
    ModelAliasDeleteResponse, ModelAliasEntry, ModelAliasListResponse, ModelAliasUpdateRequest,
    ModelAliasUpdateResponse, ModelsUnifiedListResponse, NimCatalogListResponse,
    NimContainerEntry, ServiceManifestDeployRequest, ServiceManifestDeployResponse, UnifiedModel,
    UnifiedModelInstance,
    // Addons + Notes + Translate + Users (theme 7 recovery).
    AddonAdminOnlySetRequest, AddonAdminOnlySetResponse, AddonConfigField, AddonConfigGetRequest,
    AddonConfigGetResponse, AddonConfigSetRequest, AddonConfigSetResponse, AddonDetailRequest,
    AddonDetailResponse, AddonInfo, AddonInstallRequest, AddonInstallResponse, AddonLogEntry,
    AddonLogsRequest, AddonLogsResponse, AddonNetworkRuleDecl, AddonNetworkRulesGetRequest,
    AddonNetworkRulesGetResponse, AddonNetworkRulesSetRequest, AddonNetworkRulesSetResponse,
    AddonOAuthAuthorizeStartRequest, AddonOAuthAuthorizeStartResponse,
    AddonOAuthConfigClearSecretRequest, AddonOAuthConfigClearSecretResponse,
    AddonOAuthConfigListRequest, AddonOAuthConfigListResponse, AddonOAuthConfigRow,
    AddonOAuthConfigSetRequest, AddonOAuthConfigSetResponse, AddonOAuthLinkedAccountsRequest,
    AddonOAuthLinkedAccountsResponse, AddonOAuthProviderDecl, AddonOAuthReauthorizeRequest,
    AddonOAuthReauthorizeResponse, AddonOAuthRevokeRequest, AddonOAuthRevokeResponse,
    AddonOAuthTestConnectionRequest, AddonOAuthTestConnectionResponse, AddonPermissionCatalogRequest,
    AddonPermissionCatalogResponse, AddonPermissionChangedEvent, AddonPermissionCheckRequest,
    AddonPermissionCheckResponse, AddonPermissionDecl, AddonPermissionDefault,
    AddonPermissionDefaultSetRequest, AddonPermissionDefaultSetResponse,
    AddonPermissionMatrixRequest, AddonPermissionMatrixResponse, AddonPermissionRow,
    AddonPermissionSetRequest, AddonPermissionSetResponse, AddonReloadRequest,
    AddonReloadResponse, AddonResourcesGetRequest, AddonResourcesGetResponse,
    AddonResourcesSetRequest, AddonResourcesSetResponse, AddonShowInCatalogSetRequest,
    AddonShowInCatalogSetResponse, AddonToggleRequest, AddonToggleResponse, AddonToolDecl,
    AddonToolParam, AddonToolsRequest, AddonToolsResponse, AddonUninstallRequest,
    AddonUninstallResponse, AddonVisibilityListRequest, AddonVisibilityListResponse,
    AddonVisibilityRow, AddonVisibilitySetRequest, AddonVisibilitySetResponse, AddonsListResponse,
    MyOAuthAccountsListRequest, MyOAuthAccountsListResponse, MyOAuthEntry, NoteCreateRequest,
    NoteCreateResponse, NoteDeleteRequest, NoteDeleteResponse, NoteDetailRequest,
    NoteDetailResponse, NoteEntry, NoteSetPinnedRequest, NoteSetPinnedResponse, NoteUpdateRequest,
    NoteUpdateResponse, NotesListRequest, NotesListResponse, NotesRequest, NotesResponse,
    MeetingActiveSessionRequest, MeetingActiveSessionResponse, MeetingPayload,
    MeetingSessionDescriptor, MeetingSessionDetailRequest, MeetingSessionDetailResponse,
    MeetingSessionLeaveRequest, MeetingSessionLeaveResponse, MeetingSessionListRequest,
    MeetingSessionListResponse, MeetingSessionStartRequest, MeetingSessionStartResponse,
    MeetingSessionSummaryEntry, MeetingSettingKv, MeetingSettingsGetRequest,
    MeetingSettingsGetResponse, MeetingSettingsUpdateRequest, MeetingSettingsUpdateResponse,
    MeetingSummaryGenerateRequest, MeetingSummaryGenerateResponse, MeetingTranscriptEntry,
    MeetingTranscriptsListRequest, MeetingTranscriptsListResponse,
    TranslateRequest, TranslateResponse, UserInfo, UserOAuthAccountRow, UsersListResponse,
};
