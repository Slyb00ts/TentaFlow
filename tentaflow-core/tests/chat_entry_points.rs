//! Every AI entry point records its runs on the Events log: the chat
//! (`route_chat_completion_stream` with a picked flow, and the
//! `FlowInvokeRequest` stream handler), `/v1`, addons, dashboard helpers and
//! the system curator (all of which reach the router with their own origin), and
//! a direct model call, which has no flow. For each, the rows must be stored
//! under the run's own request id (never the client's session id), carry the
//! organisation, and keep the full step content.
//!
//! Own test binary because `progress_log::start` publishes a process-wide log.

use std::time::{Duration, Instant};

use futures::StreamExt;
use tentaflow_core::api::openai::types::ChatCompletionResponse;
use tentaflow_core::auth::acl::UserContext;
use tentaflow_core::dispatch::state::AppState;
use tentaflow_core::dispatch::subscription::{
    find_stream_handler, SubscriptionEvent, SubscriptionKey, SubscriptionRegistry,
};
use tentaflow_core::dispatch::{HandlerContext, RequestOrigin};
use tentaflow_core::events::{self, EventKind, EventPayload, StoredEvent};
use tentaflow_core::flow_engine::dispatcher::{FlowActor, FlowOrigin};
use tentaflow_core::flow_engine::progress_broker::global_broker;
use tentaflow_core::routing::direct_run::DirectRun;
use tentaflow_core::routing::streaming::ChatFlowSelector;
use tentaflow_core::services::rbac::OrgContext;
use tentaflow_protocol::{FlowInputValue, FlowInvokeRequest, MessageBody, SessionAuth};

const FLOW_ID: &str = "flow-chat-entry";
const SESSION: &str = "conversation-entry";
const FLOW_JSON: &str = r#"{
    "nodes":[
        {"id":"t1","type":"trigger","config":{}},
        {"id":"o1","type":"output","config":{}}
    ],
    "edges":[{"from":"t1","to":"o1","from_port":"text","to_port":"text"}]
}"#;

fn run_ids_of(pool: &tentaflow_core::db::DbPool, origin: &str, org: Option<&str>) -> Vec<String> {
    let conn = pool.read().expect("events read");
    let mut stmt = conn
        .prepare("SELECT DISTINCT run_id FROM run_events WHERE origin = ?1 AND org_id IS ?2")
        .expect("prepare");
    stmt.query_map(rusqlite::params![origin, org], |row| {
        row.get::<_, String>(0)
    })
    .expect("query")
    .map(|r| r.expect("row"))
    .collect()
}

/// The single finished run of `origin` filed under `org`.
async fn finished_run(
    pool: &tentaflow_core::db::DbPool,
    origin: &str,
    org: Option<&str>,
) -> (String, Vec<StoredEvent>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let runs = run_ids_of(pool, origin, org);
        let mut finished = Vec::new();
        for run_id in &runs {
            let rows = events::read_run(pool, run_id, 0, 1000).expect("read run");
            if rows.iter().any(|r| r.kind == EventKind::RequestFinished) {
                finished.push((run_id.clone(), rows));
            }
        }
        // Done only when every run seen has ended: a second, nested run would
        // be on the log by the time the outer one finishes, and is waited for.
        if !runs.is_empty() && finished.len() == runs.len() {
            assert_eq!(runs.len(), 1, "exactly one top-level run: {runs:?}");
            return finished.remove(0);
        }
        assert!(
            Instant::now() < deadline,
            "no finished {origin} run for {org:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn assert_recorded_in_full(run_id: &str, rows: &[StoredEvent], origin: &str, org: Option<&str>) {
    assert_ne!(run_id, SESSION, "stored under the run, not the session id");
    assert_eq!(
        rows.first().map(|r| r.kind),
        Some(EventKind::RequestStarted)
    );
    assert_eq!(
        rows.last().map(|r| r.kind),
        Some(EventKind::RequestFinished)
    );
    assert!(rows.iter().all(|r| r.run_id == run_id));
    assert!(rows.iter().all(|r| r.origin == origin));
    assert!(rows.iter().all(|r| r.org_id.as_deref() == org));
    let io: Vec<_> = rows
        .iter()
        .filter(|r| r.kind == EventKind::StepIo)
        .collect();
    assert!(!io.is_empty(), "the step row is kept: {rows:?}");
    for row in io {
        let EventPayload::StepIo {
            z,
            input_bytes,
            output_bytes,
            ..
        } = &row.payload
        else {
            unreachable!()
        };
        let body = events::step_io::unpack(z).expect("unpack");
        assert!(
            !body.input.is_empty() || !body.output.is_empty(),
            "bodies are stored in full: {body:?}"
        );
        assert!(*input_bytes + *output_bytes > 0);
    }
}

fn org_context(org: &str) -> tentaflow_core::compliance::ai_gateway::AiGatewayContext {
    tentaflow_core::compliance::ai_gateway::AiGatewayContext {
        org_id: Some(org.to_string()),
        ..Default::default()
    }
}

/// The text of the first choice of a stored direct-run answer.
fn first_choice(output: &str) -> serde_json::Value {
    let answer: serde_json::Value = serde_json::from_str(output).expect("answer is JSON");
    answer["choices"][0]["message"].clone()
}

fn request(stream: bool) -> tentaflow_core::api::openai::types::ChatCompletionRequest {
    serde_json::from_value(serde_json::json!({
        "model": "m",
        "messages": [{"role": "user", "content": "the user's prompt"}],
        "stream": stream,
        "memory_options": {"session_id": SESSION},
    }))
    .expect("request")
}

async fn route_picked_flow(
    state: &AppState,
    origin: FlowOrigin,
    actor: FlowActor,
    user: Option<UserContext>,
    context: Option<tentaflow_core::compliance::ai_gateway::AiGatewayContext>,
) {
    let routed = state
        .router
        .route_chat_completion_stream(
            request(true),
            user,
            origin,
            actor,
            context,
            ChatFlowSelector::FlowId(FLOW_ID.to_string()),
        )
        .await
        .expect("routed");
    let mut stream = routed.response;
    while stream.next().await.is_some() {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_chat_entry_points_record_their_runs_under_the_run_and_the_org() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::for_test();
    state
        .db
        .write()
        .expect("db lock")
        .execute(
            "INSERT INTO flows (id, name, flow_json, status) VALUES (?1, 'chat entry', ?2, 'active')",
            rusqlite::params![FLOW_ID, FLOW_JSON],
        )
        .expect("seed flow");
    let pool = events::db::init(&tmp.path().join("events.db")).expect("events db");
    events::progress_log::start(pool.clone());
    let mut session_watcher = global_broker().subscribe(SESSION);

    // 1. The chat: a picked flow, the user's own organisation.
    route_picked_flow(
        &state,
        FlowOrigin::Chat,
        FlowActor::user("user-route"),
        Some(UserContext::new("user-route", "admin").with_org("org-route")),
        None,
    )
    .await;
    let (run_id, rows) = finished_run(&pool, "chat", Some("org-route")).await;
    assert_recorded_in_full(&run_id, &rows, "chat", Some("org-route"));

    // 2b. `/v1`: a user-bound key; the organisation is the user's default one.
    let bound = tentaflow_core::db::repository::create_user_account(
        &state.db,
        "v1-user",
        "hash",
        "v1-user",
        "v1-user@example.test",
    )
    .expect("user");
    tentaflow_core::services::org::repo::add_membership(
        &state.db,
        tentaflow_core::services::org::DEFAULT_ORG_ID,
        &bound,
        "role-org-viewer",
        &bound,
    )
    .expect("membership");
    route_picked_flow(
        &state,
        FlowOrigin::Api,
        FlowActor::api_key("key-1", Some(bound.clone())),
        Some(UserContext::new(bound.clone(), "admin")),
        None,
    )
    .await;
    let default_org = tentaflow_core::services::org::DEFAULT_ORG_ID;
    let (run_id, rows) = finished_run(&pool, "api", Some(default_org)).await;
    assert_recorded_in_full(&run_id, &rows, "api", Some(default_org));

    // 2c. An addon: the organisation comes from its gateway context.
    route_picked_flow(
        &state,
        FlowOrigin::Addon,
        FlowActor::system(),
        None,
        Some(tentaflow_core::compliance::ai_gateway::AiGatewayContext {
            org_id: Some("org-addon".to_string()),
            ..Default::default()
        }),
    )
    .await;
    let (run_id, rows) = finished_run(&pool, "addon", Some("org-addon")).await;
    assert_recorded_in_full(&run_id, &rows, "addon", Some("org-addon"));

    // 2d. A dashboard helper and the system curator.
    route_picked_flow(
        &state,
        FlowOrigin::Dashboard,
        FlowActor::user("user-dash"),
        Some(UserContext::new("user-dash", "admin").with_org("org-dash")),
        None,
    )
    .await;
    let (run_id, rows) = finished_run(&pool, "dashboard", Some("org-dash")).await;
    assert_recorded_in_full(&run_id, &rows, "dashboard", Some("org-dash"));
    route_picked_flow(&state, FlowOrigin::System, FlowActor::system(), None, None).await;
    let (run_id, rows) = finished_run(&pool, "system", None).await;
    assert_recorded_in_full(&run_id, &rows, "system", None);

    // 3. The FlowInvokeRequest stream handler.
    let mut user_id = [0u8; 16];
    user_id[0] = 0xFE;
    let ctx = HandlerContext {
        session: SessionAuth::UserSession {
            user_id,
            role: Some("admin".to_string()),
        },
        correlation_id: 1,
        connection_id: 0,
        resume_secret: None,
        state: state.clone(),
        origin: RequestOrigin::Local,
        org_context: Some(OrgContext {
            user_id: "user-invoke".to_string(),
            org_id: "org-invoke".to_string(),
            role_id: "role-test".to_string(),
            permissions: Default::default(),
        }),
    };
    let registry = SubscriptionRegistry::new();
    let (sub, mut rx) = registry.create(SubscriptionKey::new(0, 1), None);
    let handler = find_stream_handler("FlowInvokeRequest").expect("handler");
    (handler.handler_fn)(
        MessageBody::FlowInvokeRequestBody(FlowInvokeRequest {
            flow_id: Some(FLOW_ID.to_string()),
            model: "m".to_string(),
            service_type: "chat".to_string(),
            inputs: vec![FlowInputValue::Text("the user's prompt".to_string())],
            language: None,
            session_id: Some(SESSION.to_string()),
            output_audio: false,
            stt_model: None,
            tts_model: None,
        }),
        ctx,
        sub,
    );
    loop {
        match tokio::time::timeout(Duration::from_secs(20), rx.recv()).await {
            Ok(Some(SubscriptionEvent::End(_))) | Ok(None) => break,
            Ok(Some(SubscriptionEvent::Error(e))) => panic!("invoke failed: {e:?}"),
            Ok(Some(SubscriptionEvent::Chunk(_))) => {}
            Err(_) => panic!("the invoke stream never ended"),
        }
    }
    let (run_id, rows) = finished_run(&pool, "chat", Some("org-invoke")).await;
    assert!(run_id.starts_with("flowinvoke-"), "{run_id}");
    assert_recorded_in_full(&run_id, &rows, "chat", Some("org-invoke"));

    // 4. A direct model call has no flow; it is recorded by hand as one `llm`
    // step carrying the full prompt and answer.
    let user = UserContext::new("user-direct", "admin").with_org("org-direct");
    let run = DirectRun::begin(
        &request(false),
        FlowOrigin::Api,
        &FlowActor::user("user-direct"),
        Some(&user),
        None,
        None,
        Some("corr-direct".to_string()),
    )
    .expect("an Api call is recorded");
    let response: ChatCompletionResponse = serde_json::from_value(serde_json::json!({
        "id": "r1", "object": "chat.completion", "created": 0, "model": "served-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "the model's answer"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10},
    }))
    .expect("response");
    run.finish_response(&response);
    let (run_id, rows) = finished_run(&pool, "api", Some("org-direct")).await;
    let kinds: Vec<_> = rows.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        vec![
            EventKind::RequestStarted,
            EventKind::StepStart,
            EventKind::StepEnd,
            EventKind::StepIo,
            EventKind::RequestFinished
        ],
        "{rows:?}"
    );
    assert!(rows.iter().all(|r| r.run_id == run_id));
    let EventPayload::StepIo {
        z,
        prompt_tokens,
        completion_tokens,
        served_model,
        ..
    } = &rows[3].payload
    else {
        panic!("not a step_io row");
    };
    let body = events::step_io::unpack(z).expect("unpack");
    assert!(body.input.contains("the user's prompt"), "{}", body.input);
    assert_eq!(first_choice(&body.output)["content"], "the model's answer");
    assert_eq!(
        (*prompt_tokens, *completion_tokens, served_model.as_deref()),
        (Some(7), Some(3), Some("served-model"))
    );

    // A streamed direct call closes with its stream, and a stream dropped
    // before its end closes as cancelled.
    let chunk = |text: &str| -> tentaflow_core::api::openai::types::ChatCompletionChunk {
        serde_json::from_value(serde_json::json!({
            "id": "c", "object": "chat.completion.chunk", "created": 0, "model": "served-model",
            "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}],
        }))
        .expect("chunk")
    };
    let run = DirectRun::begin(
        &request(true),
        FlowOrigin::Camera,
        &FlowActor::system(),
        None,
        None,
        Some(&org_context("org-stream")),
        None,
    )
    .expect("recorded");
    let inner: std::pin::Pin<
        Box<dyn futures::Stream<Item = tentaflow_core::error::Result<_>> + Send>,
    > = Box::pin(futures::stream::iter(vec![
        Ok(chunk("the model's ")),
        Ok(chunk("answer")),
    ]));
    let mut recorded = run.record_stream(inner);
    while recorded.next().await.is_some() {}
    let (_, rows) = finished_run(&pool, "camera", Some("org-stream")).await;
    let io = rows
        .iter()
        .find(|r| r.kind == EventKind::StepIo)
        .expect("step_io");
    let EventPayload::StepIo { z, .. } = &io.payload else {
        unreachable!()
    };
    assert_eq!(
        first_choice(&events::step_io::unpack(z).expect("unpack").output)["content"],
        "the model's answer"
    );

    let run = DirectRun::begin(
        &request(true),
        FlowOrigin::Meeting,
        &FlowActor::system(),
        None,
        None,
        Some(&org_context("org-cut")),
        None,
    )
    .expect("recorded");
    let inner: std::pin::Pin<
        Box<dyn futures::Stream<Item = tentaflow_core::error::Result<_>> + Send>,
    > = Box::pin(futures::stream::iter(vec![Ok(chunk("partial"))]));
    let mut recorded = run.record_stream(inner);
    let _ = recorded.next().await;
    drop(recorded);
    let (_, rows) = finished_run(&pool, "meeting", Some("org-cut")).await;
    let EventPayload::RequestFinished { status, .. } = &rows.last().expect("rows").payload else {
        panic!("not a request_finished row");
    };
    assert_eq!(status, "cancelled");

    // The whole answer is kept: every choice, the reasoning, the tool calls.
    let run = DirectRun::begin(
        &request(false),
        FlowOrigin::Scheduler,
        &FlowActor::system(),
        None,
        None,
        Some(&org_context("org-tools")),
        None,
    )
    .expect("recorded");
    let response: ChatCompletionResponse = serde_json::from_value(serde_json::json!({
        "id": "r2", "object": "chat.completion", "created": 0, "model": "served-model",
        "choices": [
            {"index": 0, "finish_reason": "tool_calls", "message": {
                "role": "assistant", "content": null, "reasoning_content": "thinking it over",
                "tool_calls": [{"id": "call-1", "type": "function",
                    "function": {"name": "lookup", "arguments": "{\"q\":\"x\"}"}}]}},
            {"index": 1, "finish_reason": "stop", "message": {
                "role": "assistant", "content": "the second choice"}}
        ],
    }))
    .expect("response");
    run.finish_response(&response);
    let (_, rows) = finished_run(&pool, "scheduler", Some("org-tools")).await;
    let io = rows
        .iter()
        .find(|r| r.kind == EventKind::StepIo)
        .expect("step_io");
    let EventPayload::StepIo { z, .. } = &io.payload else {
        unreachable!()
    };
    let output = events::step_io::unpack(z).expect("unpack").output;
    let answer: serde_json::Value = serde_json::from_str(&output).expect("JSON");
    assert_eq!(
        answer["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "lookup"
    );
    assert_eq!(
        answer["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{\"q\":\"x\"}"
    );
    assert_eq!(
        answer["choices"][0]["message"]["reasoning_content"],
        "thinking it over"
    );
    assert_eq!(
        answer["choices"][1]["message"]["content"],
        "the second choice"
    );

    // A non-streaming call dropped before its answer closes as cancelled.
    let run = DirectRun::begin(
        &request(false),
        FlowOrigin::Process,
        &FlowActor::system(),
        None,
        None,
        Some(&org_context("org-drop")),
        None,
    )
    .expect("recorded");
    drop(run);
    let (_, rows) = finished_run(&pool, "process", Some("org-drop")).await;
    let EventPayload::RequestFinished { status, .. } = &rows.last().expect("rows").payload else {
        panic!("not a request_finished row");
    };
    assert_eq!(status, "cancelled");
    let io = rows
        .iter()
        .find(|r| r.kind == EventKind::StepIo)
        .expect("the prompt is kept even when no answer came");
    let EventPayload::StepIo { z, .. } = &io.payload else {
        unreachable!()
    };
    assert!(events::step_io::unpack(z)
        .expect("unpack")
        .input
        .contains("the user's prompt"));

    // A call made inside a run that is on the log opens none, and neither does
    // a bus call.
    let nested = tentaflow_core::compliance::ai_gateway::AiGatewayContext {
        parent_run: Some(
            tentaflow_core::flow_engine::dispatchers::ParentRun::unrecorded(FlowOrigin::Addon),
        ),
        ..org_context("org-nested")
    };
    assert!(DirectRun::begin(
        &request(false),
        FlowOrigin::Addon,
        &FlowActor::system(),
        None,
        None,
        Some(&nested),
        None,
    )
    .is_none());
    assert!(DirectRun::begin(
        &request(false),
        FlowOrigin::Bus,
        &FlowActor::system(),
        None,
        None,
        None,
        None,
    )
    .is_none());

    // A user in two organisations: the one the session acts in, never the oldest.
    let user_id = tentaflow_core::db::repository::create_user_account(
        &state.db,
        "two-orgs",
        "hash",
        "Two orgs",
        "two-orgs@example.test",
    )
    .expect("user");
    let org_b = tentaflow_core::services::org::repo::create_organization(
        &state.db, "Org B", "org-b", None, None, None, None,
    )
    .expect("org b")
    .org_id;
    for org in [
        tentaflow_core::services::org::DEFAULT_ORG_ID,
        org_b.as_str(),
    ] {
        tentaflow_core::services::org::add_membership(
            &state.db,
            org,
            &user_id,
            "role-org-viewer",
            "test",
        )
        .expect("membership");
    }
    let two_orgs = UserContext::new(&user_id, "user");
    for (context, expected) in [
        (Some(org_context(&org_b)), Some(org_b.as_str())),
        (None, None),
    ] {
        let run = DirectRun::begin(
            &request(false),
            FlowOrigin::Project,
            &FlowActor::user(&user_id),
            Some(&two_orgs),
            Some(&state.db),
            context.as_ref(),
            None,
        )
        .expect("recorded");
        run.finish_failed();
        let (_, rows) = finished_run(&pool, "project", expected).await;
        assert!(rows.iter().all(|r| r.org_id.as_deref() == expected));
    }

    assert!(
        session_watcher.try_recv().is_err(),
        "no entry point publishes on the client's session id"
    );
    events::progress_log::stop();
}
