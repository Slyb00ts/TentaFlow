// ===== File: dispatch/run_events.rs — RunEvents subscribe stream handler (§3.11 C) =====
//
// A streaming handler (R-STREAM): the dashboard sends one
// `AgentRunEventsSubscribeRequest{scope}` and receives a long-lived stream of
// `AgentRunEvent` frames drained from the process-global `ProgressBroker`. The
// stream stays open until the client cancels (MetaCancelStream) or disconnects
// (the writer task drops the subscription, the broker prunes the scope on its
// next publish). Events are ephemeral — the UI reconciles backlog from
// `RunDetail` after a reconnect, never from here.
//
// ACL (§3.3): a `Run` scope must resolve to the caller's principal or an admin;
// a `Session` scope is keyed by the caller's own authenticated id plus the
// client-minted conversation id (low entropy, time-correlated, so never an
// authorization token on its own). The events of a run the caller started are
// mirrored to that key by the server, so another user on the same conversation
// id holds a different key and sees nothing: no binding to hijack and no
// existence to leak. A
// non-admin subscribing to another principal's run gets an immediate
// stream end with an error frame rather than a silent empty stream.

use std::sync::Arc;

use tentaflow_protocol::{
    AgentRunEvent, AgentRunEventScope, AgentsPayload, MessageBody, ProtocolError,
    ProtocolErrorCode, SessionAuth,
};
use tokio::sync::broadcast::error::RecvError;

use crate::flow_engine::dispatchers::ProgressEvent;
use crate::flow_engine::progress_broker::global_broker;

use super::subscription::{push_chunk_async, push_end, StreamHandlerMeta, Subscription};
use super::{HandlerContext, SessionAuthKind};

/// Translates one engine `ProgressEvent` into the wire `AgentRunEvent`. The
/// `scope` the event arrived under is stamped so a multiplexed subscriber routes
/// it; kind-specific fields are filled, the rest stay at their defaults.
///
/// The emission instant the broker now stamps onto every event
/// (`StampedProgressEvent::at_ms`) is DROPPED here, and deliberately not faked
/// back in: `AgentRunEvent` has no time field, so a dashboard reading this
/// stream can only date an event by when it arrived. Carrying it would mean
/// adding a field to the protocol crate, which is a wire-contract change, not a
/// change to this handler. The durable timeline is where a timing comes from
/// (`events::metrics`); this stream is a live view.
fn to_wire(scope: &str, event: ProgressEvent) -> AgentRunEvent {
    let mut e = AgentRunEvent {
        scope: scope.to_string(),
        ..Default::default()
    };
    match event {
        ProgressEvent::NodeStarted { node_id, node_type } => {
            e.kind = "node_started".into();
            e.node_id = node_id;
            e.node_type = node_type;
        }
        ProgressEvent::NodeFinished { node_id, status } => {
            e.kind = "node_finished".into();
            e.node_id = node_id;
            e.status = status;
        }
        ProgressEvent::FirstToken { node_id } => {
            e.kind = "first_token".into();
            e.node_id = node_id;
        }
        // The content of a step goes to the event log only; a live view of a
        // run has no use for a prompt-sized body on every node.
        ProgressEvent::NodeIo { node_id, .. } => {
            e.kind = "node_io".into();
            e.node_id = node_id;
        }
        ProgressEvent::ToolIo { call_id, .. } => {
            e.kind = "tool_io".into();
            e.call_id = call_id;
        }
        ProgressEvent::NodeModel { node_id, .. } => {
            e.kind = "node_model".into();
            e.node_id = node_id;
        }
        ProgressEvent::IterationStarted { node_id, n, max } => {
            e.kind = "iteration_started".into();
            e.node_id = node_id;
            e.n = n;
            e.max = max;
        }
        ProgressEvent::IterationFinished { node_id, n } => {
            e.kind = "iteration_finished".into();
            e.node_id = node_id;
            e.n = n;
        }
        ProgressEvent::MapElement {
            node_id,
            index,
            total,
            status,
        } => {
            e.kind = "map_element".into();
            e.node_id = node_id;
            e.index = index;
            e.total = total;
            e.status = status;
        }
        ProgressEvent::ToolCallStarted { call_id, name } => {
            e.kind = "tool_call_started".into();
            e.call_id = call_id;
            e.name = name;
        }
        ProgressEvent::ToolCallFinished {
            call_id,
            name,
            status,
        } => {
            e.kind = "tool_call_finished".into();
            e.call_id = call_id;
            e.name = name;
            e.status = status;
        }
        ProgressEvent::Compaction { node_id } => {
            e.kind = "compaction".into();
            e.node_id = node_id;
        }
        ProgressEvent::ChildSpawned { run_id, agent } => {
            e.kind = "child_spawned".into();
            e.run_id = run_id;
            e.agent = agent;
        }
        ProgressEvent::ChildFinished { run_id, status } => {
            e.kind = "child_finished".into();
            e.run_id = run_id;
            e.status = status;
        }
        ProgressEvent::RouterDecision {
            node_id,
            selected,
            reason,
        } => {
            e.kind = "router_decision".into();
            e.node_id = node_id;
            e.selected = selected;
            e.reason = reason;
        }
        ProgressEvent::UserQuestion {
            run_id,
            interaction_id,
            question,
            choices,
        } => {
            e.kind = "user_question".into();
            e.run_id = run_id;
            e.interaction_id = interaction_id;
            e.question = question;
            e.choices = choices;
        }
        ProgressEvent::PermissionRequest {
            run_id,
            interaction_id,
            addon_id,
            tool_name,
            permission,
        } => {
            e.kind = "permission_request".into();
            e.run_id = run_id;
            e.interaction_id = interaction_id;
            e.addon_id = addon_id;
            e.tool_name = tool_name;
            e.permission = permission;
        }
        ProgressEvent::InteractionResolved {
            run_id,
            interaction_id,
            outcome,
        } => {
            e.kind = "interaction_resolved".into();
            e.run_id = run_id;
            e.interaction_id = interaction_id;
            e.outcome = outcome;
        }
        // The end of the run: a live view needs it to stop showing the run as
        // going. The widget settles the run from it and adds no step.
        ProgressEvent::RunFinished { run_id, status } => {
            e.kind = "run_finished".into();
            e.run_id = run_id;
            e.status = status;
        }
        // Never reaches the wire: the subscribe loop drops it. It is a
        // bookkeeping event for the durable event log, and the agent activity
        // view would render it as a nameless step.
        ProgressEvent::NodeFailed { .. } => {}
    }
    e
}

fn session_is_admin(ctx: &HandlerContext) -> bool {
    matches!(
        &ctx.session,
        SessionAuth::UserSession { role: Some(r), .. } if r == "admin"
    )
}

/// Hyphenated UUID of the calling principal, or a `PolicyDenied` error when the
/// connection is not a user session. Run-events ACL is principal-scoped, so a
/// non-session caller is rejected for every non-admin scope.
fn actor_id(ctx: &HandlerContext) -> Result<String, ProtocolError> {
    match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            Ok(uuid::Uuid::from_bytes(*user_id).hyphenated().to_string())
        }
        _ => Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "run events require a session",
        )),
    }
}

/// Resolves the scope to its broadcast key after an ACL check. Returns the key
/// to subscribe under, or an error to send as an immediate stream end.
fn authorize_scope(
    ctx: &HandlerContext,
    scope: &AgentRunEventScope,
) -> Result<String, ProtocolError> {
    match scope {
        // The conversation id is client-minted and low-entropy, so it never
        // addresses a scope by itself: the key is derived from the caller's own
        // authenticated id, and a runs' events reach it through the mirror the
        // server set up when the caller started that run. Another user's view of
        // the same conversation id is a different key and receives nothing.
        AgentRunEventScope::Session { session_id } => {
            if session_id.is_empty() {
                return Err(ProtocolError::bad_request("empty session scope"));
            }
            let actor = actor_id(ctx)?;
            Ok(crate::flow_engine::progress_broker::session_scope_key(
                &actor, session_id,
            ))
        }
        AgentRunEventScope::Run { run_id } => {
            if run_id.is_empty() {
                return Err(ProtocolError::bad_request("empty run scope"));
            }
            let actor = actor_id(ctx)?;
            let run = crate::db::repository::get_agent_run(&ctx.state.db, run_id)
                .map_err(|e| ProtocolError::internal(format!("run lookup failed: {e}")))?
                .ok_or_else(|| {
                    ProtocolError::not_found(format!("agent run not found: {run_id}"))
                })?;
            if !session_is_admin(ctx) && run.user_id.as_deref() != Some(actor.as_str()) {
                // Do not leak existence — report as not-found like the run views.
                return Err(ProtocolError::not_found(format!(
                    "agent run not found: {run_id}"
                )));
            }
            Ok(run_id.clone())
        }
    }
}

fn run_events_subscribe_handler(req: MessageBody, ctx: HandlerContext, sub: Arc<Subscription>) {
    let request = match req {
        MessageBody::AgentsBody(AgentsPayload::RunEventsSubscribeRequest(r)) => r,
        _ => {
            let _ = push_end(
                &sub,
                Some(MessageBody::Error(ProtocolError::bad_request(
                    "expected AgentRunEventsSubscribeRequest",
                ))),
            );
            return;
        }
    };

    let scope_key = match authorize_scope(&ctx, &request.scope) {
        Ok(k) => k,
        Err(err) => {
            let _ = push_end(&sub, Some(MessageBody::Error(err)));
            return;
        }
    };

    let mut rx = global_broker().subscribe(&scope_key);
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(stamped) => {
                    if matches!(stamped.event, ProgressEvent::NodeFailed { .. }) {
                        continue;
                    }
                    // An event mirrored onto the conversation's key says which run
                    // it belongs to; the key itself names no run.
                    let wire_scope = stamped.run_scope.as_deref().unwrap_or(&scope_key);
                    let mut wire = to_wire(wire_scope, stamped.event);
                    // Such a run has no `agent_runs` row, so cancel cannot reach it.
                    wire.unmanaged = stamped.run_scope.is_some();
                    let body = MessageBody::AgentsBody(AgentsPayload::RunEvent(wire));
                    // Receiver closed (client disconnected) → stop draining the
                    // broker so the scope can be pruned on its next publish.
                    if push_chunk_async(&sub, body).await.is_err() {
                        return;
                    }
                }
                // A slow subscriber that fell behind the broker ring keeps
                // listening (the UI reconciles from RunDetail); it does not end
                // the stream.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }

        // The broker scope closing is the only way this task ends with a live
        // client; emit the terminal frame so the run view is not left waiting.
        let _ = push_end(&sub, None);
    });

    // `push_end` is intentionally NOT called on this path: the subscription
    // lives until the client cancels or disconnects (the broker channel never
    // closes for a healthy session). The spawned task drains until the WS writer
    // drops the receiver, and ends the stream if the scope closes.
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "AgentRunEventsSubscribeRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: run_events_subscribe_handler,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::state::AppState;
    use tentaflow_protocol::ProtocolErrorCode;

    fn ctx(role: &str, user_id: [u8; 16], state: std::sync::Arc<AppState>) -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id,
                role: Some(role.to_string()),
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state,
            origin: crate::dispatch::RequestOrigin::Local,
            org_context: None,
        }
    }

    fn seed_run(state: &AppState, id: &str, owner: Option<&str>) {
        let params = crate::db::models::AgentParams {
            runtime_json: crate::agents::LLM_RUNTIME_JSON,
            id: "agent-x",
            name: "runner",
            display_name: None,
            description: "runs",
            system_prompt: None,
            model: None,
            tools_json: "[]",
            skills_json: "{}",
            params_json: "{}",
            max_iterations: 25,
            timeout_secs: 600,
            max_subagents: 0,
            max_spawn_depth: 1,
            flow_id: None,
            routable: true,
            is_enabled: true,
            on_child_complete: "notify",
            allowed_agents_json: None,
            actor_user_id: None,
        };
        let _ = crate::db::repository::upsert_agent(&state.db, &params);
        crate::db::repository::create_agent_run(
            &state.db,
            &crate::db::models::NewAgentRun {
                id,
                agent_id: "agent-x",
                parent_run_id: None,
                flow_execution_id: None,
                user_id: owner,
                org_id: None,
                prompt: "p",
                origin: "system",
                actor_kind: "system",
                actor_id: None,
                actor_user_id: None,
                correlation_id: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn stream_handler_is_registered_and_user_gated() {
        let meta =
            crate::dispatch::subscription::find_stream_handler("AgentRunEventsSubscribeRequest")
                .expect("run-events stream handler registered in inventory");
        assert_eq!(meta.required_auth, SessionAuthKind::UserSession);
    }

    #[test]
    fn the_end_of_a_run_reaches_the_wire_with_its_status() {
        let e = to_wire(
            "run-1",
            ProgressEvent::RunFinished {
                run_id: "run-1".into(),
                status: "failed".into(),
            },
        );
        assert_eq!(e.kind, "run_finished");
        assert_eq!(e.run_id, "run-1");
        assert_eq!(e.status, "failed");
    }

    #[test]
    fn to_wire_maps_kind_and_fields() {
        let e = to_wire(
            "sess-1",
            ProgressEvent::ToolCallFinished {
                call_id: "test-call".into(),
                name: "memory.memory_search".into(),
                status: "ok".into(),
            },
        );
        assert_eq!(e.scope, "sess-1");
        assert_eq!(e.kind, "tool_call_finished");
        assert_eq!(e.name, "memory.memory_search");
        assert_eq!(e.status, "ok");

        let q = to_wire(
            "r1",
            ProgressEvent::UserQuestion {
                run_id: "r1".into(),
                interaction_id: "q1".into(),
                question: "Which?".into(),
                choices: vec!["A".into(), "B".into()],
            },
        );
        // The live view highlights the streaming node the moment it produces
        // something, so the kind string and the node it names are part of the
        // wire contract. The NUMBER is not read from here — TTFT is
        // `request_started -> first_token` over the stored timeline, and this
        // stream carries no timestamps to subtract.
        let ft = to_wire(
            "sess-1",
            ProgressEvent::FirstToken {
                node_id: "llm-1".into(),
            },
        );
        assert_eq!(ft.kind, "first_token");
        assert_eq!(ft.node_id, "llm-1");

        assert_eq!(q.kind, "user_question");
        assert_eq!(q.run_id, "r1");
        assert_eq!(q.choices, vec!["A".to_string(), "B".to_string()]);
    }

    /// A non-admin on its own session scope receives the permission request
    /// and the question of a run it started; another user on the same
    /// conversation id receives nothing.
    #[tokio::test]
    async fn session_scope_is_keyed_by_the_caller_and_receives_its_runs() {
        let state = AppState::for_test();
        let owner_id = uuid::Uuid::from_bytes([3u8; 16]).hyphenated().to_string();
        let owner_ctx = ctx("user", [3u8; 16], state.clone());
        let other_ctx = ctx("user", [7u8; 16], state.clone());
        let scope = AgentRunEventScope::Session {
            session_id: "sess-abc".into(),
        };
        let owner_key = authorize_scope(&owner_ctx, &scope).expect("own scope");
        let other_key = authorize_scope(&other_ctx, &scope).expect("a key of its own");
        assert_ne!(owner_key, other_key);

        let broker = state.progress_broker.clone();
        let mut owner_rx = broker.subscribe(&owner_key);
        let mut other_rx = broker.subscribe(&other_key);
        broker.mirror_run_to_session(
            "run-1",
            &crate::flow_engine::progress_broker::session_scope_key(&owner_id, "sess-abc"),
        );
        for event in [
            ProgressEvent::PermissionRequest {
                run_id: "run-1".into(),
                interaction_id: "i1".into(),
                addon_id: "a".into(),
                tool_name: "t".into(),
                permission: "p".into(),
            },
            ProgressEvent::UserQuestion {
                run_id: "run-1".into(),
                interaction_id: "i2".into(),
                question: "q".into(),
                choices: vec![],
            },
        ] {
            broker.publish("run-1", event);
        }
        let first = owner_rx.recv().await.expect("permission request");
        let second = owner_rx.recv().await.expect("question");
        assert!(matches!(first.event, ProgressEvent::PermissionRequest { .. }));
        assert!(matches!(second.event, ProgressEvent::UserQuestion { .. }));
        assert!(other_rx.try_recv().is_err());
    }

    #[test]
    fn run_scope_owner_allowed_other_denied_as_not_found() {
        let state = AppState::for_test();
        let owner = uuid::Uuid::from_bytes([5u8; 16]).hyphenated().to_string();
        seed_run(&state, "run-1", Some(&owner));

        // Owner resolves.
        let owner_ctx = ctx("user", [5u8; 16], state.clone());
        let key = authorize_scope(
            &owner_ctx,
            &AgentRunEventScope::Run {
                run_id: "run-1".into(),
            },
        )
        .expect("owner allowed");
        assert_eq!(key, "run-1");

        // A different principal is rejected as not-found (no existence leak).
        let other_ctx = ctx("user", [6u8; 16], state.clone());
        let err = authorize_scope(
            &other_ctx,
            &AgentRunEventScope::Run {
                run_id: "run-1".into(),
            },
        )
        .expect_err("other denied");
        assert_eq!(err.code, ProtocolErrorCode::NotFound);

        // Admin sees any run.
        let admin_ctx = ctx("admin", [9u8; 16], state);
        authorize_scope(
            &admin_ctx,
            &AgentRunEventScope::Run {
                run_id: "run-1".into(),
            },
        )
        .expect("admin allowed");
    }
}
