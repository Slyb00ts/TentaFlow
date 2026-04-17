// =============================================================================
// Plik: message_body.rs
// Opis: Bootstrap 10 wariantow MessageBody (bootstrap). MessageBody to tresc
//       envelope'u — rkyv-serializowana osobno i trzymana jako Vec<u8> w polu
//       Envelope.body. Dzieki temu policy check dziala na envelope bez tykania
//       body, a dispatcher decoduje dopiero po przejsciu auth.
// Przyklad:
//   let body = MessageBody::NodeListRequest;
//   let body_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&body)?.to_vec();
//   let env = Envelope::new_direct(1, 1, message_kind::META_HEARTBEAT, body_bytes);
// =============================================================================

use rkyv::{Archive, Deserialize, Serialize};

// =============================================================================
// Pomocnicze typy (bootstrap — docelowo rozpisane per-archetype)
// =============================================================================

/// Lekki widok noda mesh dla list/overview. Pelne dane idą przez osobny NodeInfo.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NodeSummary {
    /// Ed25519 public key (32 bajty).
    pub node_id: [u8; 32],
    /// Hostname / display label.
    pub display_name: String,
    /// `online` / `offline` / `degraded`. String dla elastycznosci.
    pub status: String,
    /// Tier: `leader`, `worker`, itp.
    pub role: String,
    /// Czy to lokalny node (self-view).
    pub is_self: bool,
}

/// Lekki widok modelu w katalogu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelSummary {
    /// Np. "llama-3.2-1b-instruct".
    pub id: String,
    /// Rodzina: "llm", "tts", "stt", "embedding", itd.
    pub category: String,
    /// Silnik ktory uruchamia model: "llama-cpp", "mlx", "vllm"...
    pub engine_id: String,
    /// `ready`, `downloading`, `not-installed`.
    pub availability: String,
}

// =============================================================================
// Kody bledu protokolu
// =============================================================================

/// Ustabilizowane kody bledu dla `ProtocolError.code`. Dodatkowe (numeryczne)
/// mozna zawsze dorzucic — klient powinien obslugiwac nieznane graceful.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolErrorCode {
    /// Malformed frame, failed bytecheck, wrong schema version.
    InvalidFrame = 1,
    /// Brak autoryzacji dla tego MessageBody variant.
    PolicyDenied = 2,
    /// SessionAuth nie odpowiada minimum dla tej operacji.
    AuthRequired = 3,
    /// Adresowany node_id nieznany lub offline.
    NodeUnreachable = 4,
    /// Stream anulowany przez klienta lub server timeout.
    StreamCancelled = 5,
    /// Rate limit przekroczony per sesja.
    RateLimited = 6,
    /// Nie zaimplementowany handler dla tego variantu.
    NotImplemented = 7,
    /// Wewnetrzny blad serwera (szczegoly w `message`).
    Internal = 8,
    /// Zasoba nie znaleziono (np. NodeInfoRequest z nieznanym id).
    NotFound = 9,
    /// Niepoprawne argumenty requestu (walidacja pol).
    BadRequest = 10,
}

/// Ujednolicony blad protokolu. Zwracany jako `MessageBody::Error(..)` z flagą
/// `EnvelopeFlags::IS_ERROR` ustawioną dla szybkiego branchowania.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError {
    /// Kod ustabilizowany.
    pub code: ProtocolErrorCode,
    /// Human-readable message (en, dla klienta — lokalizacja po stronie GUI).
    pub message: String,
    /// Opcjonalny trace_id do korelacji z logami serwera.
    pub trace_id: Option<String>,
}

// =============================================================================
// API Keys (R-LIST + W-CREATE + W-DELETE archetypes, migration-map #37-#39)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ApiKeySummary {
    pub key_id: String,
    pub name: String,
    pub created_at_epoch: u64,
    pub last_used_at_epoch: Option<u64>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyCreateRequest {
    pub name: String,
    pub scopes: Vec<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyCreateResponse {
    pub key_id: String,
    /// Pelny token (widoczny TYLKO raz, w odpowiedzi na creation).
    pub token: String,
}

// =============================================================================
// Auth (W-ACTION + R-ONE archetypes, migration-map #40-#42)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuthLoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuthLoginResponse {
    pub jwt: String,
    pub user_id: [u8; 16],
    pub role: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuthMeResponse {
    pub user_id: [u8; 16],
    pub username: String,
    pub role: String,
}

// =============================================================================
// Chat streaming (R-STREAM archetyp, migration-map #43)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// "system" / "user" / "assistant" / "tool".
    pub role: String,
    pub content: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct ChatStreamRequest {
    pub model_id: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ChatStreamChunk {
    /// Partial token/fragment od modelu.
    pub delta: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ChatStreamEnd {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

// =============================================================================
// Cluster (W-UPDATE archetyp, migration-map #53)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterUpdateRequest {
    pub cluster_id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterUpdateResponse {
    pub cluster_id: String,
    pub updated_at_epoch: u64,
}

// =============================================================================
// MessageBody — wszystkie warianty
// =============================================================================

/// Enum wariantow tresci. Bootstrap (#29) zawieral 10; #36 dokladuje 10 kolejnych
/// pokrywajacych wszystkie 7 archetypow (R-ONE, R-LIST, R-STREAM, W-CREATE,
/// W-UPDATE, W-DELETE, W-ACTION). Dla kazdego variantu MUSI istniec wpis w
/// policy table (`#[policy]` proc-macro z #26).
///
/// Kazda nowa pozycja = additive change i bump `SCHEMA_VERSION`.
///
/// UWAGA: `Eq` NIE implementowane bo ChatStreamRequest ma `Option<f32>` (floaty
/// nie sa Eq przez NaN). Uzywamy `PartialEq` wszedzie.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub enum MessageBody {
    // ---- Meta (schema/handshake/keepalive) ----
    /// Klient -> serwer: sprawdz wersje protokolu przy handshake.
    MetaSchemaVersionCheck { client_version: u16 },
    /// Serwer -> klient: potwierdzenie (accepted=false => disconnect).
    MetaSchemaVersionAck { server_version: u16, accepted: bool },
    /// Dwukierunkowy keepalive (WSS ping substitute, liczy RTT).
    MetaHeartbeat { sent_at_epoch: u64 },
    /// Klient -> serwer: anuluj aktywny stream (match po correlation_id w envelope).
    MetaCancelStream,

    // ---- Read-list (R-LIST archetyp) ----
    /// Klient -> serwer: lista nodow mesh. Anonymous / UserSession / MeshTrust.
    NodeListRequest,
    /// Serwer -> klient: odpowiedz (summary, pelne info przez NodeInfoRequest).
    NodeListResponse { nodes: Vec<NodeSummary> },
    /// Klient -> serwer: lista modeli (publiczne, Anonymous OK).
    ModelListRequest,
    /// Serwer -> klient: odpowiedz.
    ModelListResponse { models: Vec<ModelSummary> },

    // ---- Read-one (R-ONE archetyp) ----
    /// Klient -> serwer: szczegoly konkretnego noda.
    NodeInfoRequest { node_id: [u8; 32] },

    // ---- API Keys (R-LIST + W-CREATE + W-DELETE) ----
    ApiKeyListRequest,
    ApiKeyListResponse { keys: Vec<ApiKeySummary> },
    ApiKeyCreateRequestBody(ApiKeyCreateRequest),
    ApiKeyCreateResponseBody(ApiKeyCreateResponse),
    ApiKeyRevokeRequest { key_id: String },
    ApiKeyRevokeResponse { deleted: bool },

    // ---- Auth (W-ACTION + R-ONE) ----
    AuthLoginRequestBody(AuthLoginRequest),
    AuthLoginResponseBody(AuthLoginResponse),
    AuthMeRequest,
    AuthMeResponseBody(AuthMeResponse),

    // ---- Chat streaming (R-STREAM) ----
    ChatStreamRequestBody(ChatStreamRequest),
    ChatStreamChunkBody(ChatStreamChunk),
    ChatStreamEndBody(ChatStreamEnd),

    // ---- Cluster (W-UPDATE) ----
    ClusterUpdateRequestBody(ClusterUpdateRequest),
    ClusterUpdateResponseBody(ClusterUpdateResponse),

    // ---- Error ----
    /// Ujednolicony blad. Towarzyszy `EnvelopeFlags::IS_ERROR`.
    Error(ProtocolError),
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_node() -> NodeSummary {
        NodeSummary {
            node_id: [5u8; 32],
            display_name: "alpha".to_string(),
            status: "online".to_string(),
            role: "leader".to_string(),
            is_self: true,
        }
    }

    fn sample_model() -> ModelSummary {
        ModelSummary {
            id: "llama-3.2-1b-instruct".to_string(),
            category: "llm".to_string(),
            engine_id: "llama-cpp".to_string(),
            availability: "ready".to_string(),
        }
    }

    fn round_trip(body: MessageBody) -> MessageBody {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&body).expect("encode");
        rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(&bytes).expect("decode")
    }

    #[test]
    fn meta_schema_version_check_round_trip() {
        let body = MessageBody::MetaSchemaVersionCheck { client_version: 2 };
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn meta_schema_version_ack_round_trip() {
        let body = MessageBody::MetaSchemaVersionAck {
            server_version: 2,
            accepted: true,
        };
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn meta_heartbeat_round_trip() {
        let body = MessageBody::MetaHeartbeat {
            sent_at_epoch: 1_700_000_000,
        };
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn meta_cancel_stream_round_trip() {
        let body = MessageBody::MetaCancelStream;
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn node_list_request_unit_variant() {
        let body = MessageBody::NodeListRequest;
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn node_list_response_with_multiple_nodes() {
        let body = MessageBody::NodeListResponse {
            nodes: vec![
                sample_node(),
                NodeSummary {
                    node_id: [6u8; 32],
                    display_name: "beta".to_string(),
                    status: "degraded".to_string(),
                    role: "worker".to_string(),
                    is_self: false,
                },
            ],
        };
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn node_info_request_round_trip() {
        let body = MessageBody::NodeInfoRequest {
            node_id: [0xAAu8; 32],
        };
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn model_list_request_round_trip() {
        let body = MessageBody::ModelListRequest;
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn model_list_response_round_trip() {
        let body = MessageBody::ModelListResponse {
            models: vec![sample_model()],
        };
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn error_round_trip_with_trace() {
        let body = MessageBody::Error(ProtocolError {
            code: ProtocolErrorCode::PolicyDenied,
            message: "requires UserSession".to_string(),
            trace_id: Some("trace-xyz".to_string()),
        });
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn error_round_trip_without_trace() {
        let body = MessageBody::Error(ProtocolError {
            code: ProtocolErrorCode::NotFound,
            message: "node not in mesh".to_string(),
            trace_id: None,
        });
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn all_error_codes_survive_round_trip() {
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
            let body = MessageBody::Error(ProtocolError {
                code,
                message: "x".to_string(),
                trace_id: None,
            });
            assert_eq!(round_trip(body.clone()), body);
        }
    }

    #[test]
    fn truncated_body_bytes_rejected() {
        let body = MessageBody::NodeListResponse {
            nodes: vec![sample_node(), sample_node()],
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&body).expect("encode");
        let half = &bytes[..bytes.len() / 2];
        assert!(rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(half).is_err());
    }

    #[test]
    fn empty_body_bytes_rejected() {
        let result = rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn api_key_crud_round_trip() {
        let list = MessageBody::ApiKeyListResponse {
            keys: vec![ApiKeySummary {
                key_id: "k1".to_string(),
                name: "primary".to_string(),
                created_at_epoch: 1_700_000_000,
                last_used_at_epoch: Some(1_700_100_000),
            }],
        };
        assert_eq!(round_trip(list.clone()), list);

        let create = MessageBody::ApiKeyCreateRequestBody(ApiKeyCreateRequest {
            name: "svc".to_string(),
            scopes: vec!["read".to_string(), "write".to_string()],
        });
        assert_eq!(round_trip(create.clone()), create);

        let created = MessageBody::ApiKeyCreateResponseBody(ApiKeyCreateResponse {
            key_id: "k2".to_string(),
            token: "secret-only-shown-once".to_string(),
        });
        assert_eq!(round_trip(created.clone()), created);

        let revoke = MessageBody::ApiKeyRevokeRequest {
            key_id: "k2".to_string(),
        };
        assert_eq!(round_trip(revoke.clone()), revoke);

        let revoked = MessageBody::ApiKeyRevokeResponse { deleted: true };
        assert_eq!(round_trip(revoked.clone()), revoked);
    }

    #[test]
    fn auth_login_flow_round_trip() {
        let login = MessageBody::AuthLoginRequestBody(AuthLoginRequest {
            username: "admin".to_string(),
            password: "s3cret".to_string(),
        });
        assert_eq!(round_trip(login.clone()), login);

        let logged = MessageBody::AuthLoginResponseBody(AuthLoginResponse {
            jwt: "eyJ...".to_string(),
            user_id: [9u8; 16],
            role: "admin".to_string(),
        });
        assert_eq!(round_trip(logged.clone()), logged);

        let me = MessageBody::AuthMeRequest;
        assert_eq!(round_trip(me.clone()), me);

        let me_resp = MessageBody::AuthMeResponseBody(AuthMeResponse {
            user_id: [9u8; 16],
            username: "admin".to_string(),
            role: "admin".to_string(),
        });
        assert_eq!(round_trip(me_resp.clone()), me_resp);
    }

    #[test]
    fn chat_stream_round_trip() {
        let req = MessageBody::ChatStreamRequestBody(ChatStreamRequest {
            model_id: "llama-3.2".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: "You are helpful.".to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: "Hi".to_string(),
                },
            ],
            temperature: Some(0.7),
            max_tokens: Some(256),
        });
        assert_eq!(round_trip(req.clone()), req);

        let chunk = MessageBody::ChatStreamChunkBody(ChatStreamChunk {
            delta: "Hello".to_string(),
        });
        assert_eq!(round_trip(chunk.clone()), chunk);

        let end = MessageBody::ChatStreamEndBody(ChatStreamEnd {
            prompt_tokens: 12,
            completion_tokens: 34,
        });
        assert_eq!(round_trip(end.clone()), end);
    }

    #[test]
    fn cluster_update_round_trip() {
        let req = MessageBody::ClusterUpdateRequestBody(ClusterUpdateRequest {
            cluster_id: "dev".to_string(),
            name: "Development".to_string(),
            description: Some("Internal cluster".to_string()),
        });
        assert_eq!(round_trip(req.clone()), req);

        let resp = MessageBody::ClusterUpdateResponseBody(ClusterUpdateResponse {
            cluster_id: "dev".to_string(),
            updated_at_epoch: 1_700_200_000,
        });
        assert_eq!(round_trip(resp.clone()), resp);
    }

    #[test]
    fn body_nests_inside_envelope() {
        use crate::envelope::{message_kind, Envelope};
        let body = MessageBody::NodeListRequest;
        let body_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&body)
            .expect("encode body")
            .to_vec();
        let env = Envelope::new_direct(1, 1, message_kind::META_HEARTBEAT, body_bytes);
        let env_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&env).expect("encode env");
        let decoded_env: Envelope =
            rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&env_bytes).expect("decode env");
        let decoded_body: MessageBody =
            rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(&decoded_env.body)
                .expect("decode body");
        assert_eq!(decoded_body, body);
    }
}
