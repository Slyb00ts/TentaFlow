// ===== File: tests/openai_embeddings.rs — /v1/embeddings gateway contract: request
// parsing (string / array / token ids / empty), vendor-field passthrough to an
// OpenAI-compatible HTTP backend (wiremock), usage propagation, and the client-
// facing wire encoding (`encoding_format` float vs base64). =====

use std::sync::Arc;

use serde_json::{json, Value};
use tentaflow_core::api::openai::server::parse_embedding_request;
use tentaflow_core::api::openai::types::{
    encode_embedding_base64, EmbeddingEncoding, EmbeddingInput, EmbeddingRequest,
};
use tentaflow_core::config::{ConnectionType, ServiceBackend};
use tentaflow_core::inference::InferenceManager;
use tentaflow_core::services::backend::BackendClient;
use tentaflow_core::services::catalog::CatalogProvider;
use tentaflow_core::services::handles_cache::{BackendHandle, LiveHandlesCache};
use tentaflow_core::services::mesh_registry::MeshServicesRegistry;
use tentaflow_core::services::runtime::{AliasResolver, ExecutionContext, ModelRuntimeExecutor};
use tentaflow_protocol::{RequestTimeParameters, ServiceInfo, ServiceModelEntry};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const NODE_ID: &str = "test-node";
const SERVICE_ID: i64 = 1;
const MODEL_NAME: &str = "emb-model";

fn service_info(endpoint_url: &str) -> ServiceInfo {
    ServiceInfo {
        id: SERVICE_ID,
        node_id: NODE_ID.to_string(),
        engine_id: "test-http-embeddings".to_string(),
        category: "embeddings".to_string(),
        display_name: "Embeddings test backend".to_string(),
        deploy_method: "external".to_string(),
        transport: "http".to_string(),
        status: "running".to_string(),
        pinned: true,
        paused: false,
        runtime_pid: None,
        runtime_port: None,
        sidecar_quic_port: None,
        endpoint_url: Some(endpoint_url.to_string()),
        restart_count: 0,
        health_last_err: None,
        active_deploy_id: "deploy-1".to_string(),
        last_deploy_id: "deploy-1".to_string(),
        deployment_progress_pct: 100,
        progress_message: None,
        models: vec![ServiceModelEntry {
            model_name: MODEL_NAME.to_string(),
            display_name: None,
            capabilities: Vec::new(),
            context_length: None,
            quantization: None,
            is_default: true,
            service_surfaces: Vec::new(),
        }],
        update_available: false,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        request_time_parameters: RequestTimeParameters::default(),
        usage_json: None,
        usage_updated_at: None,
        gpu_selection: String::new(),
        cluster_deployment_id: String::new(),
    }
}

fn http_handle(base_url: &str) -> BackendHandle {
    let backend = ServiceBackend {
        connection: ConnectionType::OpenAIApi {
            url: format!("{base_url}/v1"),
            api_key: Some("test-key".into()),
            api_key_env: None,
            extra_headers: Vec::new(),
            custom_endpoint: None,
            request_format: None,
            tts_config: None,
        },
        max_concurrent: 2,
        timeout_ms: 5_000,
        weight: 1,
        model_name_override: None,
        health_check_path: None,
    };
    BackendHandle::Http(Arc::new(
        BackendClient::new(backend, None).expect("backend client"),
    ))
}

/// Executor with ONE catalog entry (`emb-model`, surface Embeddings from the
/// `embeddings` category) backed by the wiremock HTTP service.
fn executor_for(server: &MockServer) -> ModelRuntimeExecutor {
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory DB");
    tentaflow_core::db::migrations::run(&conn).expect("migrations");
    let pool: tentaflow_core::db::DbPool = Arc::new(tentaflow_core::db::Db::from_connection(conn));

    let registry = MeshServicesRegistry::new();
    registry.replace_local(NODE_ID.to_string(), vec![service_info(&server.uri())]);
    let catalog = Arc::new(CatalogProvider::new());
    catalog.rebuild(&registry, &pool).expect("catalog rebuild");

    let handles = Arc::new(LiveHandlesCache::new());
    handles.insert(NODE_ID.to_string(), SERVICE_ID, http_handle(&server.uri()));
    let resolver = Arc::new(AliasResolver::new(
        handles,
        Arc::new(|| NODE_ID.to_string()),
    ));
    let local_inference = Arc::new(
        tentaflow_core::inference::local::LocalInferenceHandler::new(Arc::new(
            tokio::sync::RwLock::new(InferenceManager::new()),
        )),
    );
    ModelRuntimeExecutor::new(
        catalog,
        resolver,
        None,
        local_inference,
        Arc::new(parking_lot::RwLock::new(None)),
        Arc::new(parking_lot::RwLock::new(None)),
        Arc::new(parking_lot::RwLock::new(None)),
        Some(pool),
    )
}

/// Exactly what an OpenAI SDK client posts: array input, `dimensions`, `user`
/// and vendor extras the gateway does not model.
fn client_body() -> Value {
    json!({
        "model": MODEL_NAME,
        "input": ["first text", "second text"],
        "encoding_format": "base64",
        "dimensions": 4,
        "user": "end-user-7",
        "truncate": "END",
        "input_type": "query"
    })
}

const V0: [f32; 4] = [0.25, -1.5, 3.141_592_7, f32::MIN_POSITIVE];
const V1: [f32; 4] = [1.0, 2.0, -3.0, 4.5];

fn assert_forwarded(body: &Value) {
    assert_eq!(body["model"], json!(MODEL_NAME));
    assert_eq!(body["input"], json!(["first text", "second text"]));
    assert_eq!(body["dimensions"], json!(4));
    assert_eq!(body["user"], json!("end-user-7"));
    assert_eq!(body["truncate"], json!("END"));
    assert_eq!(body["input_type"], json!("query"));
    // Transport encoding toward the backend is always base64 (one decode per
    // vector); the client's own `encoding_format` is applied at the edge.
    assert_eq!(body["encoding_format"], json!("base64"));
}

fn backend_response(base64: bool) -> Value {
    let vec = |v: &[f32], idx: u32| {
        let embedding = if base64 {
            json!(encode_embedding_base64(v))
        } else {
            json!(v)
        };
        json!({"object": "embedding", "index": idx, "embedding": embedding})
    };
    json!({
        "object": "list",
        "model": "internal/backend-name",
        "data": [vec(&V0, 0), vec(&V1, 1)],
        "usage": {"prompt_tokens": 7, "total_tokens": 7}
    })
}

async fn run_through_executor(base64_backend: bool) -> (Value, Value) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).expect("json body");
            assert_forwarded(&body);
            ResponseTemplate::new(200).set_body_json(backend_response(base64_backend))
        })
        .expect(1)
        .mount(&server)
        .await;

    let executor = executor_for(&server);
    let request: EmbeddingRequest =
        parse_embedding_request(&serde_json::to_vec(&client_body()).unwrap()).expect("valid");
    let encoding = EmbeddingEncoding::parse(request.encoding_format.as_deref()).unwrap();
    let mut ctx = ExecutionContext::default();
    let response = executor
        .execute_embeddings(request, &mut ctx)
        .await
        .expect("embeddings through HTTP backend");

    assert_eq!(ctx.route_metadata.backend_type.as_deref(), Some("http"));
    assert_eq!(response.object, "list");
    assert_eq!(response.data.len(), 2);
    assert_eq!(response.usage.prompt_tokens, 7);
    assert_eq!(response.usage.total_tokens, 7);
    for (got, want) in response.data[0].embedding.iter().zip(V0.iter()) {
        assert_eq!(got.to_bits(), want.to_bits(), "vector 0 bit-exact");
    }
    for (got, want) in response.data[1].embedding.iter().zip(V1.iter()) {
        assert_eq!(got.to_bits(), want.to_bits(), "vector 1 bit-exact");
    }

    let wire_requested: Value = serde_json::from_slice(&response.to_wire_json(encoding)).unwrap();
    let wire_float: Value =
        serde_json::from_slice(&response.to_wire_json(EmbeddingEncoding::Float)).unwrap();
    (wire_requested, wire_float)
}

/// Backend answers base64 (vLLM / OpenAI with encoding_format=base64): client
/// asked for base64 → gets base64 strings; float rendering stays available.
#[tokio::test]
async fn base64_request_yields_base64_response_with_usage() {
    let (b64, floats) = run_through_executor(true).await;

    assert_eq!(b64["object"], "list");
    assert_eq!(b64["usage"]["prompt_tokens"], 7);
    assert_eq!(b64["usage"]["total_tokens"], 7);
    assert_eq!(b64["data"][0]["object"], "embedding");
    assert_eq!(b64["data"][0]["index"], 0);
    assert_eq!(b64["data"][1]["index"], 1);
    assert_eq!(
        b64["data"][0]["embedding"],
        json!(encode_embedding_base64(&V0))
    );
    assert_eq!(
        b64["data"][1]["embedding"],
        json!(encode_embedding_base64(&V1))
    );

    let arr = floats["data"][1]["embedding"]
        .as_array()
        .expect("float array");
    assert_eq!(arr.len(), 4);
    assert_eq!(arr[3], json!(4.5));
}

/// Backend that ignores `encoding_format` and answers float arrays must still
/// decode, and a base64-requesting client still gets base64 at the edge.
#[tokio::test]
async fn float_backend_response_is_decoded_and_reencoded() {
    let (b64, floats) = run_through_executor(false).await;
    assert_eq!(
        b64["data"][0]["embedding"],
        json!(encode_embedding_base64(&V0))
    );
    assert!(floats["data"][0]["embedding"].is_array());
}

/// Backend usage missing / zero is surfaced as-is (the router-level counter
/// falls back to total_tokens); a 5xx from the backend is an executor error,
/// not a panic.
#[tokio::test]
async fn backend_error_becomes_executor_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .expect(1)
        .mount(&server)
        .await;
    let executor = executor_for(&server);
    let request = EmbeddingRequest {
        model: MODEL_NAME.to_string(),
        input: EmbeddingInput::Single("x".into()),
        encoding_format: None,
        dimensions: None,
        user: None,
        extra: serde_json::Map::new(),
    };
    let err = executor
        .execute_embeddings(request, &mut ExecutionContext::default())
        .await
        .expect_err("500 from backend must fail");
    assert!(err.to_string().contains("500"), "got: {err}");
}

#[test]
fn request_parsing_contract() {
    let ok = |v: Value| parse_embedding_request(&serde_json::to_vec(&v).unwrap());

    let single = ok(json!({"model": "m", "input": "hello"})).unwrap();
    assert!(matches!(single.input, EmbeddingInput::Single(ref s) if s == "hello"));
    assert_eq!(single.input.len(), 1);

    let multi = ok(json!({"model": "m", "input": ["a", "b", "c"]})).unwrap();
    assert!(matches!(multi.input, EmbeddingInput::Multiple(ref v) if v.len() == 3));

    // Token-id inputs: explicit 400 message, not an opaque serde error.
    let tokens = ok(json!({"model": "m", "input": [1, 2, 3]})).unwrap_err();
    assert!(tokens.contains("token ids"), "got: {tokens}");
    let token_batches = ok(json!({"model": "m", "input": [[1, 2], [3]]})).unwrap_err();
    assert!(token_batches.contains("token ids"), "got: {token_batches}");

    assert!(ok(json!({"model": "m", "input": ""}))
        .unwrap_err()
        .contains("empty string"));
    assert!(ok(json!({"model": "m", "input": []}))
        .unwrap_err()
        .contains("empty array"));
    assert!(ok(json!({"model": "m", "input": ["a", ""]}))
        .unwrap_err()
        .contains("empty strings"));
    assert!(ok(json!({"model": "m"})).unwrap_err().contains("input"));
    assert!(ok(json!({"model": "m", "input": {"text": "x"}})).is_err());
    assert!(parse_embedding_request(b"{not json").is_err());

    assert_eq!(EmbeddingEncoding::parse(Some("binary")), None);
}
