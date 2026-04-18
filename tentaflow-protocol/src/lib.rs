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
    ApiKeyCreateRequest, ApiKeyCreateResponse, ApiKeySummary, AuditEvent, AuthLoginRequest,
    AuthLoginResponse, AuthMeResponse, ChatMessage, ChatStreamChunk, ChatStreamEnd,
    ChatStreamRequest, ClusterUpdateRequest, ClusterUpdateResponse, DashboardSnapshot,
    FlowCreateRequest, FlowDetail, FlowExecutionSummary, FlowSummary, HubDownloadProgress,
    HubEngineSummary, HubModelSearchResult, MeshPairInitRequest, MeshPairInitResponse,
    MeshPeerSummary, MeshTrustRevokedEvent, MeshTrustedKeysSyncEvent, MessageBody,
    ModelDetail, ModelInstallRequest, ModelSummary, NodeSummary, PromptDetail, PromptSummary,
    ProtocolError, ProtocolErrorCode, RegistrySummary, ServiceDeployProgress,
    ServiceDeployRequest, ServiceSummary, SettingEntry, SettingsUpdateRequest,
};
