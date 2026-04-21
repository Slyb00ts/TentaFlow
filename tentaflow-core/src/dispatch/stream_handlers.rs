// =============================================================================
// Plik: dispatch/stream_handlers.rs
// Opis: Streaming handlery (R-STREAM archetyp). Inaczej niz sync handlery
//       (handlers.rs), te spawnuja task emitujacy serie chunkow przez
//       SubscriptionEvent::Chunk + final SubscriptionEvent::End. ws_binary
//       writer task drainuje mpsc i pakuje w IS_STREAM_CHUNK/IS_STREAM_END
//       envelope flags.
//
// Bootstrap: ChatStreamRequest emituje 3 chunki "Hello", " world", "!"
// na potrzeby testow — prawdziwa integracja z LLM przyjdzie w #36 phase 2
// gdy router/inference bedzie wystawial async stream.
// =============================================================================

use std::sync::Arc;

use tentaflow_protocol::{ChatStreamChunk, ChatStreamEnd, MessageBody, SessionAuth};

use super::recorder;
use super::resume_token::{self, ResumeError};
use super::subscription::{push_chunk, push_end, StreamHandlerMeta, Subscription};
use super::{HandlerContext, SessionAuthKind};

// =============================================================================
// ChatStreamRequest — emituje 3 demo chunki + end.
// =============================================================================

fn chat_stream_handler(_req: MessageBody, _ctx: HandlerContext, sub: Arc<Subscription>) {
    tokio::spawn(async move {
        let demo_chunks = ["Hello", ", world", "!"];
        for delta in demo_chunks {
            if push_chunk(
                &sub,
                MessageBody::ChatStreamChunkBody(ChatStreamChunk {
                    delta: delta.to_string(),
                }),
            )
            .is_err()
            {
                // Subscriber odpadl (writer task zamknal mpsc).
                return;
            }
        }
        let _ = push_end(
            &sub,
            Some(MessageBody::ChatStreamEndBody(ChatStreamEnd {
                prompt_tokens: 5,
                completion_tokens: 3,
            })),
        );
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
    async fn chat_stream_handler_emits_three_chunks_and_end() {
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

        let mut chunks = 0;
        let mut got_end = false;
        while let Some(evt) = rx.recv().await {
            match evt {
                SubscriptionEvent::Chunk(MessageBody::ChatStreamChunkBody(_)) => chunks += 1,
                SubscriptionEvent::End(_) => {
                    got_end = true;
                    break;
                }
                other => panic!("unexpected event: {:?}", other),
            }
        }
        assert_eq!(chunks, 3);
        assert!(got_end);
    }
}
