// ===== File: tests/tool_calling_e2e.rs — phase-1 tool-calling outcome: prompt-mode
// post-processing extracts and coerces <tool_call> blocks (unit level, no model), and
// a real GGUF model called through ModelRuntimeExecutor::execute_chat with one tool
// spec produces a parsed tool_call (ignored e2e, env TENTAFLOW_LLAMA_TEST_MODEL). =====

use tentaflow_core::api::openai::types::{
    ChatCompletionResponse, Choice, FunctionDefinition, Message, MessageContent, Tool,
};
use tentaflow_core::services::runtime::tool_calling;

/// Tool spec shaped like the bundled `memory.memory_store` addon tool —
/// one required string and one integer the model tends to emit as a string.
fn memory_store_tool() -> Tool {
    Tool {
        tool_type: "function".to_string(),
        function: FunctionDefinition {
            name: "memory.memory_store".to_string(),
            description: Some(
                "Store a fact, preference or piece of information in memory.".to_string(),
            ),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "fact": { "type": "string", "description": "The information to store." },
                    "importance": { "type": "integer", "description": "1-10 priority." }
                },
                "required": ["fact"]
            })),
        },
    }
}

fn assistant_response(text: &str) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: "chatcmpl-test".to_string(),
        object: "chat.completion".to_string(),
        created: 1,
        model: "tool-test-model".to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text(text.to_string())),
                ..Default::default()
            },
            finish_reason: Some("stop".to_string()),
            logprobs: None,
        }],
        usage: None,
        system_fingerprint: None,
        transcribed_text: None,
        speaker_id: None,
        speaker_name: None,
        speaker_confidence: None,
        detected_intent: None,
        detected_tools: None,
    }
}

/// Stage-B post-processing on a canned completion: the `<tool_call>` block
/// must come out as a full OpenAI tool call with schema-coerced arguments,
/// finish_reason flipped to "tool_calls" and the block removed from the text.
#[test]
fn prompt_mode_postprocessing_extracts_and_coerces_tool_call() {
    let tools = vec![memory_store_tool()];
    let mut response = assistant_response(
        "I will remember that.\n\
         <tool_call>{\"name\":\"memory.memory_store\",\
         \"arguments\":{\"fact\":\"favorite color is blue\",\"importance\":\"5\"}}</tool_call>\n\
         Stored for you.",
    );

    tool_calling::apply_prompt_mode_response(&mut response, &tools);

    let choice = &response.choices[0];
    assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
    let calls = choice
        .message
        .tool_calls
        .as_ref()
        .expect("tool calls extracted from <tool_call> block");
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert!(call.id.starts_with("call_0_"), "deterministic call id");
    assert_eq!(call.tool_type, "function");
    assert_eq!(call.function.name, "memory.memory_store");
    let args: serde_json::Value =
        serde_json::from_str(&call.function.arguments).expect("arguments are valid JSON");
    assert_eq!(args["fact"], "favorite color is blue");
    // "5" emitted as a string must coerce to integer 5 per the schema.
    assert_eq!(args["importance"], 5);

    let Some(MessageContent::Text(content)) = &choice.message.content else {
        panic!("cleaned content must stay text");
    };
    assert!(!content.contains("<tool_call>"), "block removed from text");
    assert!(content.contains("I will remember that."));
    assert!(content.contains("Stored for you."));
}

/// Untouched responses (no `<tool_call>` markup) must pass through with the
/// original finish_reason and no tool_calls attached.
#[test]
fn prompt_mode_postprocessing_leaves_plain_answers_alone() {
    let tools = vec![memory_store_tool()];
    let mut response = assistant_response("The capital of France is Paris.");

    tool_calling::apply_prompt_mode_response(&mut response, &tools);

    let choice = &response.choices[0];
    assert_eq!(choice.finish_reason.as_deref(), Some("stop"));
    assert!(choice.message.tool_calls.is_none());
    let Some(MessageContent::Text(content)) = &choice.message.content else {
        panic!("content must stay text");
    };
    assert_eq!(content, "The capital of France is Paris.");
}

#[cfg(feature = "inference-llamacpp")]
mod llamacpp_prompt_mode {
    use std::path::PathBuf;
    use std::sync::Arc;

    use tentaflow_core::api::openai::types::{ChatCompletionRequest, Message, MessageContent};
    use tentaflow_core::inference::{DeployParamsSnapshot, InferenceManager};
    use tentaflow_core::services::catalog::CatalogProvider;
    use tentaflow_core::services::handles_cache::{BackendHandle, LiveHandlesCache};
    use tentaflow_core::services::mesh_registry::MeshServicesRegistry;
    use tentaflow_core::services::runtime::{
        AliasResolver, ExecutionContext, ModelRuntimeExecutor,
    };
    use tentaflow_protocol::{RequestTimeParameters, ServiceInfo, ServiceModelEntry};

    use super::memory_store_tool;

    const NODE_ID: &str = "test-node";
    const SERVICE_ID: i64 = 1;
    const MODEL_NAME: &str = "tool-test-model";

    fn model_path() -> Option<PathBuf> {
        std::env::var("TENTAFLOW_LLAMA_TEST_MODEL")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.exists())
    }

    fn deploy_params() -> DeployParamsSnapshot {
        let mut s = DeployParamsSnapshot::default();
        s.llamacpp
            .insert("n_gpu_layers".into(), serde_json::json!(99));
        s.llamacpp
            .insert("ctx_size".into(), serde_json::json!(4096));
        s.llamacpp.insert("n_parallel".into(), serde_json::json!(2));
        s
    }

    /// Embedded llama.cpp service row — the engine_id is deliberately not a
    /// known manifest id so the catalog derives surfaces from the "llm"
    /// category fallback (deterministic Chat surface, text-only modalities).
    fn service_info() -> ServiceInfo {
        ServiceInfo {
            id: SERVICE_ID,
            node_id: NODE_ID.to_string(),
            engine_id: "test-embedded".to_string(),
            category: "llm".to_string(),
            display_name: "Tool calling test model".to_string(),
            deploy_method: "native_embedded".to_string(),
            transport: "embedded".to_string(),
            status: "running".to_string(),
            pinned: true,
            paused: false,
            runtime_pid: None,
            runtime_port: None,
            sidecar_quic_port: None,
            endpoint_url: None,
            restart_count: 0,
            health_last_err: None,
            active_deploy_id: "deploy-1".to_string(),
            last_deploy_id: "deploy-1".to_string(),
            deployment_progress_pct: 100,
            progress_message: None,
            usage_json: None,
            usage_updated_at: None,
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
        }
    }

    fn chat_request_with_tool() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: MODEL_NAME.to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Some(MessageContent::Text(
                    "Remember that my favorite color is blue. \
                     Store it with the memory.memory_store tool."
                        .to_string(),
                )),
                ..Default::default()
            }],
            temperature: Some(0.0),
            max_tokens: Some(256),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            stream: false,
            stream_options: None,
            user: None,
            response_format: None,
            tools: Some(vec![memory_store_tool()]),
            tool_choice: None,
            n: None,
            memory_options: None,
            audio_input: None,
        }
    }

    /// Drives the SAME runtime seam stage B added: execute_chat resolves the
    /// embedded candidate, `dispatch_chat_blocking` injects the tool section
    /// into the system prompt (prompt mode) and parses `<tool_call>` blocks
    /// out of the completion.
    #[tokio::test]
    #[ignore = "needs GPU + GGUF pointed to by TENTAFLOW_LLAMA_TEST_MODEL"]
    async fn embedded_model_calls_tool_through_runtime_executor() {
        let Some(path) = model_path() else {
            eprintln!("skipped: TENTAFLOW_LLAMA_TEST_MODEL not set or missing");
            return;
        };

        let manager = Arc::new(tokio::sync::RwLock::new(InferenceManager::new()));
        manager
            .write()
            .await
            .load_model(&path, deploy_params(), Some("llamacpp"))
            .await
            .expect("load GGUF into embedded llama.cpp engine");

        let conn = rusqlite::Connection::open_in_memory().expect("in-memory DB");
        tentaflow_core::db::migrations::run(&conn).expect("migrations");
        let pool: tentaflow_core::db::DbPool =
            Arc::new(tentaflow_core::db::Db::from_connection(conn));

        let registry = MeshServicesRegistry::new();
        registry.replace_local(NODE_ID.to_string(), vec![service_info()]);
        let catalog = Arc::new(CatalogProvider::new());
        catalog
            .rebuild(&registry, &pool)
            .expect("catalog rebuild from registry");

        let handles = Arc::new(LiveHandlesCache::new());
        handles.insert(
            NODE_ID.to_string(),
            SERVICE_ID,
            BackendHandle::Embedded {
                model_name: MODEL_NAME.to_string(),
                node_id: NODE_ID.to_string(),
                engine_id: "test-embedded".to_string(),
            },
        );
        let resolver = Arc::new(AliasResolver::new(
            handles,
            Arc::new(|| NODE_ID.to_string()),
        ));
        let local_inference =
            Arc::new(tentaflow_core::inference::local::LocalInferenceHandler::new(manager.clone()));
        let executor = ModelRuntimeExecutor::new(
            catalog,
            resolver,
            None,
            local_inference,
            Arc::new(parking_lot::RwLock::new(None)),
            Arc::new(parking_lot::RwLock::new(None)),
            Arc::new(parking_lot::RwLock::new(None)),
            None,
        );

        let mut ctx = ExecutionContext::default();
        let response = executor
            .execute_chat(chat_request_with_tool(), &mut ctx)
            .await
            .expect("execute_chat through the embedded candidate");

        assert_eq!(
            ctx.route_metadata.backend_type.as_deref(),
            Some("embedded"),
            "request must be served by the embedded (prompt-mode) branch"
        );

        let choice = &response.choices[0];
        let tool_calls = choice.message.tool_calls.clone().unwrap_or_default();
        if tool_calls.is_empty() {
            panic!(
                "model emitted no parsable tool call; finish_reason={:?}; raw output: {:?}",
                choice.finish_reason, choice.message.content
            );
        }
        assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
        let call = &tool_calls[0];
        assert_eq!(
            call.function.name, "memory.memory_store",
            "only advertised tool; raw output: {:?}",
            choice.message.content
        );
        let args: serde_json::Value = serde_json::from_str(&call.function.arguments)
            .unwrap_or_else(|e| {
                panic!(
                    "tool call arguments are not valid JSON ({e}); raw arguments: {}",
                    call.function.arguments
                )
            });
        assert!(
            args.is_object(),
            "arguments must be a JSON object, got: {args}"
        );

        manager
            .write()
            .await
            .unload_model()
            .await
            .expect("unload model releases resources");
    }
}
