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

use tentaflow_protocol::{ChatStreamChunk, ChatStreamEnd, MessageBody};

use super::subscription::{push_chunk, push_end, Subscription, StreamHandlerMeta};
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
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::super::subscription::{find_stream_handler, stream_handler_count, SubscriptionEvent};
    use super::super::SessionAuthKind;

    #[test]
    fn chat_stream_handler_registered() {
        assert!(stream_handler_count() >= 1);
        let h = find_stream_handler("ChatStreamRequest").unwrap();
        assert_eq!(h.required_auth, SessionAuthKind::UserSession);
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
            session: SessionAuth::UserSession { user_id: [0u8; 16] },
            correlation_id: 1,
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
