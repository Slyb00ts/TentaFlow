// =============================================================================
// Plik: dispatch/stream_handlers.rs
// Opis: Streaming handlery (R-STREAM archetyp). Inaczej niz sync handlery
//       (handlers.rs), te spawnuja task emitujacy serie chunkow przez
//       SubscriptionEvent::Chunk + final SubscriptionEvent::End. ws_binary
//       writer task drainuje mpsc i pakuje w IS_STREAM_CHUNK/IS_STREAM_END
//       envelope flags.
// =============================================================================

use std::sync::Arc;

use futures::StreamExt;
use tentaflow_protocol::{ChatStreamChunk, ChatStreamEnd, MessageBody, SessionAuth};

use super::recorder;
use super::resume_token::{self, ResumeError};
use super::subscription::{
    push_chunk, push_chunk_async, push_end, push_end_async, StreamHandlerMeta, Subscription,
};
use super::{HandlerContext, SessionAuthKind};

// =============================================================================
// ChatStreamRequest — real SSE streaming z Router. Bierze ChatStreamRequest
// (model_id + messages[] + temperature + max_tokens), konstruuje OpenAI-shape
// ChatCompletionRequest z stream=true, woła Router::route_chat_completion_stream
// i forwarduje kazdy Delta.content jako ChatStreamChunk. Router sam wybiera
// backend: flow engine → QUIC mesh → HTTP backend (dynamic, np. vllm-metal na
// 127.0.0.1:8000) → local inference fallback.
// =============================================================================

fn chat_stream_handler(req: MessageBody, ctx: HandlerContext, sub: Arc<Subscription>) {
    use crate::api::openai::types::{ChatCompletionRequest, Message, MessageContent};

    let stream_req = match req {
        MessageBody::ChatStreamRequestBody(r) => r,
        _ => {
            let _ = push_end(
                &sub,
                Some(MessageBody::ChatStreamEndBody(ChatStreamEnd {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                })),
            );
            return;
        }
    };

    let router = ctx.state.router.clone();
    tokio::spawn(async move {
        let messages: Vec<Message> = stream_req
            .messages
            .iter()
            .map(|m| Message {
                role: m.role.clone(),
                content: Some(MessageContent::Text(m.content.clone())),
                ..Default::default()
            })
            .collect();

        // Przekazujemy request 1:1 z GUI — bez forced system prompt i bez
        // override sampling defaults. Backend (LocalInference / vLLM) zna
        // swoje sane defaults; wstrzykiwanie ENG system prompt do polskich
        // 4-bit modeli (Bielik) degradowalo kontekst → bełkot z corpusu.
        let request = ChatCompletionRequest {
            model: stream_req.model_id.clone(),
            messages,
            temperature: stream_req.temperature,
            max_tokens: stream_req.max_tokens,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            stream: true,
            stream_options: None,
            user: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            n: None,
            memory_options: None,
            audio_input: None,
        };

        let route_result = match router.route_chat_completion_stream(request, None).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("chat_stream: route_chat_completion_stream failed: {:#}", e);
                let _ = push_chunk(
                    &sub,
                    MessageBody::ChatStreamChunkBody(ChatStreamChunk {
                        delta: format!("[routing error] {}", e),
                    }),
                );
                let _ = push_end(
                    &sub,
                    Some(MessageBody::ChatStreamEndBody(ChatStreamEnd {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                    })),
                );
                return;
            }
        };

        let mut stream = route_result.response;
        let mut completion_tokens: u32 = 0;
        // State machine: backend (vLLM/parser) wydziela chain-of-thought do
        // `delta.reasoning_content`, content do `delta.content`. Frontend
        // (chat.js) parsuje `<think>...</think>` jako collapsed block, więc
        // bridge musi opakować reasoning w te tagi. Otwieramy `<think>` na
        // pierwszym reasoning chunku, zamykamy `</think>` przy przejściu
        // na content lub na finish (gdyby reasoning był ostatni).
        let mut in_thinking = false;
        while let Some(chunk_res) = stream.next().await {
            let chunk = match chunk_res {
                Ok(c) => c,
                Err(e) => {
                    let _ = push_chunk_async(
                        &sub,
                        MessageBody::ChatStreamChunkBody(ChatStreamChunk {
                            delta: format!("\n[stream error] {}", e),
                        }),
                    )
                    .await;
                    break;
                }
            };
            if let Some(choice) = chunk.choices.first() {
                let reasoning = choice
                    .delta
                    .reasoning_content
                    .as_deref()
                    .filter(|s| !s.is_empty());
                let content = choice
                    .delta
                    .content
                    .as_deref()
                    .filter(|s| !s.is_empty());

                if let Some(r) = reasoning {
                    let payload = if in_thinking {
                        r.to_string()
                    } else {
                        in_thinking = true;
                        format!("<think>{}", r)
                    };
                    if push_chunk_async(
                        &sub,
                        MessageBody::ChatStreamChunkBody(ChatStreamChunk { delta: payload }),
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                    completion_tokens = completion_tokens.saturating_add(1);
                }

                if let Some(c) = content {
                    let payload = if in_thinking {
                        in_thinking = false;
                        format!("</think>{}", c)
                    } else {
                        c.to_string()
                    };
                    if push_chunk_async(
                        &sub,
                        MessageBody::ChatStreamChunkBody(ChatStreamChunk { delta: payload }),
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                    completion_tokens = completion_tokens.saturating_add(1);
                }
            }
        }
        // Cleanup: gdy reasoning był ostatni (brak content po nim), domknij
        // tag żeby front miał poprawny `<think>...</think>` parować.
        if in_thinking {
            let _ = push_chunk_async(
                &sub,
                MessageBody::ChatStreamChunkBody(ChatStreamChunk {
                    delta: "</think>".to_string(),
                }),
            )
            .await;
        }

        let _ = push_end_async(
            &sub,
            Some(MessageBody::ChatStreamEndBody(ChatStreamEnd {
                prompt_tokens: 0,
                completion_tokens,
            })),
        )
        .await;
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "ChatStreamRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: chat_stream_handler,
    }
}

// =============================================================================
// ClusterProbeStreamRequest — streaming probe miedzy nodami klastra.
// Wysyla "started" → seria "probing_pair"/"result" → "complete" + End z agregatami.
// =============================================================================

fn cluster_probe_stream_handler(req: MessageBody, ctx: HandlerContext, sub: Arc<Subscription>) {
    use tentaflow_protocol::{
        ClusterProbeStreamChunk, ClusterProbeStreamEnd, ClusterProbeStreamRequest,
    };

    tokio::spawn(async move {
        let payload: ClusterProbeStreamRequest = match req {
            MessageBody::ClusterProbeStreamRequestBody(p) => p,
            _ => {
                let _ = push_end(
                    &sub,
                    Some(MessageBody::ClusterProbeStreamEndBody(
                        ClusterProbeStreamEnd {
                            total_pairs: 0,
                            successful: 0,
                            failed: 0,
                        },
                    )),
                );
                return;
            }
        };

        // Walidacja minimum 2 nody.
        if payload.node_ids.len() < 2 {
            let _ = push_chunk(
                &sub,
                MessageBody::ClusterProbeStreamChunkBody(ClusterProbeStreamChunk {
                    event_type: "complete".into(),
                    source_node: None,
                    target_node: None,
                    success: None,
                    latency_ms: None,
                    bandwidth_mbps: None,
                    interface_type: None,
                    message: Some("minimum 2 nodes required".into()),
                }),
            );
            let _ = push_end(
                &sub,
                Some(MessageBody::ClusterProbeStreamEndBody(
                    ClusterProbeStreamEnd {
                        total_pairs: 0,
                        successful: 0,
                        failed: 0,
                    },
                )),
            );
            return;
        }

        // Started.
        if push_chunk(
            &sub,
            MessageBody::ClusterProbeStreamChunkBody(ClusterProbeStreamChunk {
                event_type: "started".into(),
                source_node: None,
                target_node: None,
                success: None,
                latency_ms: None,
                bandwidth_mbps: None,
                interface_type: None,
                message: Some(format!("probing {} nodes", payload.node_ids.len())),
            }),
        )
        .is_err()
        {
            return;
        }

        let qm = ctx.state.quic_mesh.clone();
        let local_id = ctx.state.local_node_id.to_string();

        let mut total_pairs: u32 = 0;
        let mut successful: u32 = 0;
        let mut failed: u32 = 0;

        // Iteruj po wszystkich uporzadkowanych parach (i, j) i = a, j = b.
        for i in 0..payload.node_ids.len() {
            for j in (i + 1)..payload.node_ids.len() {
                let a = payload.node_ids[i].clone();
                let b = payload.node_ids[j].clone();
                total_pairs += 1;

                // probing_pair event.
                if push_chunk(
                    &sub,
                    MessageBody::ClusterProbeStreamChunkBody(ClusterProbeStreamChunk {
                        event_type: "probing_pair".into(),
                        source_node: Some(a.clone()),
                        target_node: Some(b.clone()),
                        success: None,
                        latency_ms: None,
                        bandwidth_mbps: None,
                        interface_type: None,
                        message: None,
                    }),
                )
                .is_err()
                {
                    return;
                }

                // Probe — uzyj QUIC RTT z iroh manager jako proxy dla latency
                // i odswiezonej peer info dla speed/interface_type.
                let (success, latency_ms, bandwidth_mbps, interface_type) = match &qm {
                    Some(qm) => {
                        let a_local = a == local_id;
                        let b_local = b == local_id;
                        let other = if a_local { b.clone() } else { a.clone() };
                        let connected = a_local || b_local || qm.is_connected(&other).await;
                        if !connected {
                            (false, None, None, None)
                        } else {
                            let rtt_us = qm.get_peer_rtt_us(&other).await.unwrap_or(0);
                            let lat_ms = ((rtt_us as f64) / 1000.0).round() as u32;
                            let peer = ctx.state.mesh_peer_store.get(&other);
                            let iface = peer.as_ref().and_then(|_p| {
                                // Peer_store nie trzyma typu interfejsu — wrocimy
                                // ethernet jako rozsadny default dla connected peer.
                                Some("ethernet".to_string())
                            });
                            (true, Some(lat_ms), None, iface)
                        }
                    }
                    None => (false, None, None, None),
                };

                if success {
                    successful += 1;
                } else {
                    failed += 1;
                }

                if push_chunk(
                    &sub,
                    MessageBody::ClusterProbeStreamChunkBody(ClusterProbeStreamChunk {
                        event_type: "result".into(),
                        source_node: Some(a),
                        target_node: Some(b),
                        success: Some(success),
                        latency_ms,
                        bandwidth_mbps,
                        interface_type,
                        message: None,
                    }),
                )
                .is_err()
                {
                    return;
                }
            }
        }

        // Complete chunk + End z agregatami.
        let _ = push_chunk(
            &sub,
            MessageBody::ClusterProbeStreamChunkBody(ClusterProbeStreamChunk {
                event_type: "complete".into(),
                source_node: None,
                target_node: None,
                success: None,
                latency_ms: None,
                bandwidth_mbps: None,
                interface_type: None,
                message: None,
            }),
        );

        let _ = push_end(
            &sub,
            Some(MessageBody::ClusterProbeStreamEndBody(
                ClusterProbeStreamEnd {
                    total_pairs,
                    successful,
                    failed,
                },
            )),
        );
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "ClusterProbeStreamRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: cluster_probe_stream_handler,
    }
}

// =============================================================================
// SubscribeResumeRequest — verify token, replay z recorder buffer, end.
// =============================================================================

fn subscribe_resume_handler(req: MessageBody, ctx: HandlerContext, sub: Arc<Subscription>) {
    tokio::spawn(async move {
        let resume_token_bytes = match &req {
            MessageBody::SubscribeResumeRequest { resume_token } => resume_token.clone(),
            _ => {
                let _ = push_end(
                    &sub,
                    Some(MessageBody::SubscribeResumeAck {
                        accepted: false,
                        error: Some("expected SubscribeResumeRequest variant".to_string()),
                    }),
                );
                return;
            }
        };

        let secret = match &ctx.resume_secret {
            Some(s) => s.clone(),
            None => {
                let _ = push_end(
                    &sub,
                    Some(MessageBody::SubscribeResumeAck {
                        accepted: false,
                        error: Some("server not configured for resume".to_string()),
                    }),
                );
                return;
            }
        };

        // P0 FIX: token musi byc zwiazany z user_id caller'a. Anonymous nie ma
        // resume capability w ogole — Anonymous nie moze otrzymac tokenu od
        // wystawiciela (nie ma user_id), wiec verify zawsze padnie.
        let caller_user_id = match &ctx.session {
            SessionAuth::UserSession { user_id, .. } => *user_id,
            _ => {
                let _ = push_end(
                    &sub,
                    Some(MessageBody::SubscribeResumeAck {
                        accepted: false,
                        error: Some("resume requires UserSession".to_string()),
                    }),
                );
                return;
            }
        };

        let token = match resume_token::verify(&resume_token_bytes, &caller_user_id, &secret) {
            Ok(t) => t,
            Err(ResumeError::Expired) => {
                let _ = push_end(
                    &sub,
                    Some(MessageBody::SubscribeResumeAck {
                        accepted: false,
                        error: Some("resume token expired".to_string()),
                    }),
                );
                return;
            }
            Err(ResumeError::SignatureMismatch) => {
                let _ = push_end(
                    &sub,
                    Some(MessageBody::SubscribeResumeAck {
                        accepted: false,
                        error: Some("resume token signature invalid".to_string()),
                    }),
                );
                return;
            }
            Err(ResumeError::InvalidLength) => {
                let _ = push_end(
                    &sub,
                    Some(MessageBody::SubscribeResumeAck {
                        accepted: false,
                        error: Some("resume token malformed".to_string()),
                    }),
                );
                return;
            }
            Err(ResumeError::UserMismatch) => {
                // P0 FIX: kluczowy check — token nalezy do innego usera, replay attack.
                let _ = push_end(
                    &sub,
                    Some(MessageBody::SubscribeResumeAck {
                        accepted: false,
                        error: Some("resume token belongs to different user".to_string()),
                    }),
                );
                return;
            }
        };

        // Token ok — emit ack jako pierwszy chunk, potem replay.
        if push_chunk(
            &sub,
            MessageBody::SubscribeResumeAck {
                accepted: true,
                error: None,
            },
        )
        .is_err()
        {
            return;
        }

        // Pobierz frame'y z recorder buffer (tylko jesli recorder zainicjalizowany).
        if let Some(rec) = recorder::global() {
            // Token zawiera last_sequence ktore klient widzial — replay zaczyna sie
            // od first frame z id > last_sequence (uproszczenie: traktujemy
            // sequence == row id, ostateczna mapa po dopiacych test e2e).
            let target_correlation = token.subscription_id as u64;
            match rec.outgoing_after(target_correlation, token.last_sequence as i64) {
                Ok(frames) => {
                    for frame in frames {
                        if let Ok(body) =
                            rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(&frame.body_bytes)
                        {
                            if push_chunk(&sub, body).is_err() {
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = push_end(
                        &sub,
                        Some(MessageBody::SubscribeResumeAck {
                            accepted: false,
                            error: Some(format!("recorder query failed: {}", e)),
                        }),
                    );
                    return;
                }
            }
        }

        // Koniec replay — klient teraz live.
        let _ = push_end(&sub, None);
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "SubscribeResumeRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: subscribe_resume_handler,
    }
}

// =============================================================================
// DeploymentLogStream — real-time log tail + phase/progress events.
// =============================================================================
// Frontend subscribes przez ApiBinary.subscribe('deploymentLogStreamRequest',
// { deployId, replayTail: true }). Handler:
//   1. Replay log_tail z DB jako serię StreamChunk {kind='log'}.
//   2. Subscribe do global log_bus (broadcast channel per deploy_id).
//   3. Dla każdego BusMessage::Line emit StreamChunk, dla End emit StreamEnd + End.
//   4. Gdy bus channel zamknięty (runner skończył): emit StreamEnd z aktualnym
//      statusem z DB + push_end.

fn deployment_log_stream_handler(req: MessageBody, ctx: HandlerContext, sub: Arc<Subscription>) {
    use tentaflow_protocol::{
        DeploymentLogStreamRequest, DeploymentPayload, DeploymentStreamChunk, DeploymentStreamEnd,
    };

    let payload = match req {
        MessageBody::DeploymentBody(DeploymentPayload::ReqLogStream(p)) => p,
        _ => {
            let _ = push_end(&sub, None);
            return;
        }
    };
    let DeploymentLogStreamRequest {
        deploy_id,
        replay_tail,
    } = payload;

    let db = ctx.state.db.clone();
    tokio::spawn(async move {
        // Replay historycznych linii — najpierw z deployments po slug,
        // fallback do legacy `deployments` jesli rekord nie istnieje w v2.
        if replay_tail {
            if let Ok(Some(v2)) = crate::services_repo::deployments::get_by_slug(&db, &deploy_id) {
                for (idx, line) in v2.log_tail.split('\n').enumerate() {
                    if line.is_empty() {
                        continue;
                    }
                    let chunk = DeploymentStreamChunk {
                        deploy_id: deploy_id.clone(),
                        kind: "log".to_string(),
                        line: line.to_string(),
                        phase: String::new(),
                        progress_pct: 0,
                        ts_ms: idx as i64,
                    };
                    if push_chunk(
                        &sub,
                        MessageBody::DeploymentBody(DeploymentPayload::StreamChunk(chunk)),
                    )
                    .is_err()
                    {
                        return;
                    }
                }
                let final_status = match v2.status {
                    crate::services_repo::deployments::DeploymentStatus::Success => "success",
                    crate::services_repo::deployments::DeploymentStatus::Failed => "failure",
                    _ => "",
                };
                if !final_status.is_empty() {
                    let end = DeploymentStreamEnd {
                        deploy_id: deploy_id.clone(),
                        final_status: final_status.to_string(),
                        image_tag: String::new(),
                        container_name: String::new(),
                        error_message: v2.error_text.unwrap_or_default(),
                        duration_ms: 0,
                    };
                    let _ = push_end(
                        &sub,
                        Some(MessageBody::DeploymentBody(DeploymentPayload::StreamEnd(
                            end,
                        ))),
                    );
                    return;
                }
            }
        }

        // Live tail z log_bus.
        let mut rx = match crate::deploy::log_bus::subscribe(&deploy_id) {
            Some(r) => r,
            None => {
                // Kanał już zamknięty — deployment albo skończony albo nie istnieje.
                // Rolę fallback pełni replay powyżej; tu po prostu end.
                let _ = push_end(&sub, None);
                return;
            }
        };

        use crate::deploy::log_bus::BusMessage;
        loop {
            match rx.recv().await {
                Ok(BusMessage::Line(line)) => {
                    let chunk = DeploymentStreamChunk {
                        deploy_id: line.deploy_id,
                        kind: line.kind,
                        line: line.line,
                        phase: line.phase,
                        progress_pct: line.progress_pct as i32,
                        ts_ms: line.ts_ms,
                    };
                    if push_chunk(
                        &sub,
                        MessageBody::DeploymentBody(DeploymentPayload::StreamChunk(chunk)),
                    )
                    .is_err()
                    {
                        return;
                    }
                }
                Ok(BusMessage::End {
                    deploy_id: did,
                    final_status,
                    image_tag,
                    container_name,
                    error_message,
                    duration_ms,
                }) => {
                    let end = DeploymentStreamEnd {
                        deploy_id: did,
                        final_status,
                        image_tag,
                        container_name,
                        error_message,
                        duration_ms,
                    };
                    let _ = push_end(
                        &sub,
                        Some(MessageBody::DeploymentBody(DeploymentPayload::StreamEnd(
                            end,
                        ))),
                    );
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    let _ = push_end(&sub, None);
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Subscriber za wolny — skip.
                    continue;
                }
            }
        }
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "DeploymentLogStreamRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: deployment_log_stream_handler,
    }
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::super::subscription::{
        find_stream_handler, stream_handler_count, SubscriptionEvent,
    };
    use super::super::SessionAuthKind;

    #[test]
    fn chat_stream_handler_registered() {
        assert!(stream_handler_count() >= 2);
        let h = find_stream_handler("ChatStreamRequest").unwrap();
        assert_eq!(h.required_auth, SessionAuthKind::UserSession);
    }

    #[test]
    fn subscribe_resume_handler_registered() {
        let h = find_stream_handler("SubscribeResumeRequest").unwrap();
        assert_eq!(h.required_auth, SessionAuthKind::UserSession);
    }

    #[tokio::test]
    async fn p0_cross_user_resume_attack_rejected() {
        use super::super::resume_token;
        use super::super::subscription::{SubscriptionEvent, SubscriptionRegistry};
        use super::super::HandlerContext;
        use std::sync::Arc;
        use tentaflow_protocol::{MessageBody, SessionAuth};

        let secret = Arc::new(b"test-secret".to_vec());
        let alice = [0xAAu8; 16];
        let bob = [0xBBu8; 16];

        // Alice's token (server wystawil dla niej).
        let alice_token = resume_token::issue(42, 5, alice, &secret);

        let reg = SubscriptionRegistry::new();
        let (sub, mut rx) = reg.create(99, None);
        let h = find_stream_handler("SubscribeResumeRequest").unwrap();

        // Bob proboje uzyc tokenu Alice.
        let req = MessageBody::SubscribeResumeRequest {
            resume_token: alice_token,
        };
        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: bob,
                role: None,
            },
            correlation_id: 99,
            resume_secret: Some(secret),
            state: super::super::state::AppState::for_test(),
        };
        (h.handler_fn)(req, ctx, sub);

        let event = rx.recv().await.expect("end with error ack");
        match event {
            SubscriptionEvent::End(Some(MessageBody::SubscribeResumeAck { accepted, error })) => {
                assert!(!accepted, "P0 fix: cross-user token must be rejected");
                let msg = error.unwrap();
                assert!(
                    msg.contains("different user"),
                    "expected user-mismatch error, got: {}",
                    msg
                );
            }
            other => panic!("expected End(SubscribeResumeAck rejected), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn subscribe_resume_handler_rejects_invalid_token() {
        use super::super::subscription::SubscriptionRegistry;
        use super::super::HandlerContext;
        use std::sync::Arc;
        use tentaflow_protocol::{MessageBody, SessionAuth};

        let reg = SubscriptionRegistry::new();
        let (sub, mut rx) = reg.create(1, None);
        let h = find_stream_handler("SubscribeResumeRequest").unwrap();
        // 80-byte token (current TOKEN_LEN) of garbage — will fail signature verify.
        let req = MessageBody::SubscribeResumeRequest {
            resume_token: vec![0u8; 80],
        };
        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: None,
            },
            correlation_id: 1,
            resume_secret: Some(Arc::new(b"test-secret".to_vec())),
            state: super::super::state::AppState::for_test(),
        };
        (h.handler_fn)(req, ctx, sub);

        let event = rx.recv().await.unwrap();
        match event {
            SubscriptionEvent::End(Some(MessageBody::SubscribeResumeAck { accepted, error })) => {
                assert!(!accepted);
                let msg = error.unwrap();
                assert!(
                    msg.contains("signature invalid") || msg.contains("different user"),
                    "expected signature/user error, got: {}",
                    msg
                );
            }
            other => panic!("expected End(SubscribeResumeAck), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn subscribe_resume_handler_accepts_valid_token() {
        use super::super::resume_token;
        use super::super::subscription::SubscriptionRegistry;
        use super::super::HandlerContext;
        use std::sync::Arc;
        use tentaflow_protocol::{MessageBody, SessionAuth};

        let secret = Arc::new(b"test-secret".to_vec());
        let user_id = [0u8; 16];
        let token = resume_token::issue(42, 5, user_id, &secret);

        let reg = SubscriptionRegistry::new();
        let (sub, mut rx) = reg.create(2, None);
        let h = find_stream_handler("SubscribeResumeRequest").unwrap();
        let req = MessageBody::SubscribeResumeRequest {
            resume_token: token,
        };
        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id,
                role: None,
            },
            correlation_id: 2,
            resume_secret: Some(secret),
            state: super::super::state::AppState::for_test(),
        };
        (h.handler_fn)(req, ctx, sub);

        // Pierwszy event: Ack accepted=true.
        let event1 = rx.recv().await.unwrap();
        match event1 {
            SubscriptionEvent::Chunk(MessageBody::SubscribeResumeAck { accepted, error: _ }) => {
                assert!(accepted);
            }
            other => panic!(
                "expected Chunk(SubscribeResumeAck accepted), got {:?}",
                other
            ),
        }
        // Drugi event: End (brak recorder = brak replay frames).
        let event2 = rx.recv().await.unwrap();
        assert!(matches!(event2, SubscriptionEvent::End(None)));
    }

    #[tokio::test]
    async fn chat_stream_handler_routes_to_router_and_emits_end() {
        // AppState::for_test() nie ma skonfigurowanych backendow LLM, wiec
        // router.route_chat_completion_stream zwroci Err → handler emituje
        // jeden chunk [routing error] i End. Test weryfikuje ze (a) request
        // w ogole jest parsowany, (b) End jest emitowany, (c) nie wystepuje
        // panika. Pelny test produkcji z backendem jest w api/openai/server.rs.
        use super::super::subscription::SubscriptionRegistry;
        use super::super::HandlerContext;
        use tentaflow_protocol::{ChatMessage, ChatStreamRequest, MessageBody, SessionAuth};

        let reg = SubscriptionRegistry::new();
        let (sub, mut rx) = reg.create(1, None);

        let h = find_stream_handler("ChatStreamRequest").unwrap();
        let req = MessageBody::ChatStreamRequestBody(ChatStreamRequest {
            model_id: "test".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "hi".into(),
            }],
            temperature: None,
            max_tokens: None,
        });
        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: None,
            },
            correlation_id: 1,
            resume_secret: None,
            state: super::super::state::AppState::for_test(),
        };
        (h.handler_fn)(req, ctx, sub);

        let mut got_end = false;
        while let Some(evt) = rx.recv().await {
            match evt {
                SubscriptionEvent::Chunk(MessageBody::ChatStreamChunkBody(_)) => {
                    // chunk z [routing error] albo realny delta — ignorujemy
                }
                SubscriptionEvent::End(_) => {
                    got_end = true;
                    break;
                }
                other => panic!("unexpected event: {:?}", other),
            }
        }
        assert!(got_end, "chat_stream_handler powinien emitowac End");
    }
}
