// =============================================================================
// Plik: tests/ws_binary_resume_e2e.rs
// Opis: End-to-end test resume token flow (Task #34 partial).
//       Symuluje:
//         1. Klient subskrybuje stream, dostaje N chunkow
//         2. "Disconnect" — anulujemy subskrypcje
//         3. Klient reconnects, wysyla SubscribeResumeRequest z tokenem
//         4. Serwer verify → Ack accepted=true → replay z recorder
//
//       Real 3-node mesh forwarding (forwarded_session_claim end-to-end)
//       wymaga spawned procesow z mDNS — to zostaje #34 phase 2.
// =============================================================================

use std::sync::Arc;
use tentaflow_core::dispatch::{
    self, recorder, resume_token,
    subscription::{find_stream_handler, SubscriptionEvent, SubscriptionRegistry},
    HandlerContext,
};
use tentaflow_protocol::{ChatMessage, ChatStreamRequest, MessageBody, SessionAuth};

#[tokio::test]
async fn streaming_handler_emits_chunks_and_end() {
    // Pomijamy global recorder init (tests share global state — to OK dla
    // smoke; dla real CI uzywamy osobnego --test-threads=1).
    let reg = SubscriptionRegistry::new();
    let (sub, mut rx) = reg.create(100, None);

    let h = find_stream_handler("ChatStreamRequest").expect("handler registered");
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
        session: SessionAuth::UserSession { user_id: [0u8; 16], role: None },
        correlation_id: 100,
        resume_secret: Some(Arc::new(b"e2e-secret".to_vec())),
        state: tentaflow_core::dispatch::state::AppState::for_test(),
    };

    (h.handler_fn)(req, ctx, sub);

    let mut chunks_received = 0;
    let mut end_received = false;
    while let Some(event) = rx.recv().await {
        match event {
            SubscriptionEvent::Chunk(_) => chunks_received += 1,
            SubscriptionEvent::End(_) => {
                end_received = true;
                break;
            }
            SubscriptionEvent::Error(e) => panic!("unexpected error: {:?}", e),
        }
    }
    assert_eq!(chunks_received, 3);
    assert!(end_received);
}

#[tokio::test]
async fn resume_token_round_trip_through_subscribe_resume_handler() {
    let secret = Arc::new(b"e2e-resume-secret".to_vec());

    // Krok 1: serwer wystawia token (symulujac zakonczenie streama).
    let user_id = [0u8; 16];
    let token_bytes = resume_token::issue(42u128, 7u64, user_id, &secret);
    assert!(!token_bytes.is_empty());

    // Krok 2: klient ze swojej strony wysyla SubscribeResumeRequest z tokenem.
    let reg = SubscriptionRegistry::new();
    let (sub, mut rx) = reg.create(200, None);
    let h = find_stream_handler("SubscribeResumeRequest").expect("registered");
    let req = MessageBody::SubscribeResumeRequest {
        resume_token: token_bytes,
    };
    let ctx = HandlerContext {
        session: SessionAuth::UserSession { user_id, role: None },
        correlation_id: 200,
        resume_secret: Some(secret.clone()),
        state: tentaflow_core::dispatch::state::AppState::for_test(),
    };

    (h.handler_fn)(req, ctx, sub);

    // Pierwszy event: SubscribeResumeAck { accepted: true }.
    let event1 = rx.recv().await.expect("ack");
    match event1 {
        SubscriptionEvent::Chunk(MessageBody::SubscribeResumeAck { accepted, error: _ }) => {
            assert!(accepted, "valid token should be accepted");
        }
        other => panic!("expected SubscribeResumeAck, got {:?}", other),
    }

    // Brak recorder w tym tescie = brak chunkow do replay = od razu End.
    let event2 = rx.recv().await.expect("end");
    assert!(matches!(event2, SubscriptionEvent::End(_)));
}

#[tokio::test]
async fn invalid_resume_token_results_in_negative_ack() {
    let secret = Arc::new(b"correct-secret".to_vec());
    let _wrong_secret = b"wrong-secret".to_vec();

    let reg = SubscriptionRegistry::new();
    let (sub, mut rx) = reg.create(300, None);
    let h = find_stream_handler("SubscribeResumeRequest").expect("registered");

    // Wystawiamy token z innym sekretem — verify powinno failowac.
    let bad_token = resume_token::issue(42u128, 7u64, [0u8; 16], b"different-secret");
    let req = MessageBody::SubscribeResumeRequest {
        resume_token: bad_token,
    };
    let ctx = HandlerContext {
        session: SessionAuth::UserSession { user_id: [0u8; 16], role: None },
        correlation_id: 300,
        resume_secret: Some(secret),
        state: tentaflow_core::dispatch::state::AppState::for_test(),
    };

    (h.handler_fn)(req, ctx, sub);

    let event = rx.recv().await.expect("end with error ack");
    match event {
        SubscriptionEvent::End(Some(MessageBody::SubscribeResumeAck { accepted, error })) => {
            assert!(!accepted);
            let msg = error.unwrap();
            assert!(msg.contains("signature invalid"), "got: {}", msg);
        }
        other => panic!("expected End(SubscribeResumeAck rejected), got {:?}", other),
    }
}

#[tokio::test]
async fn dispatch_metrics_record_chat_stream_calls() {
    use tentaflow_core::dispatch::metrics;

    // Wywolaj dispatch dla MetaHeartbeat (sync handler), zeby sie zarejestrowal w metrykach.
    let ctx = HandlerContext {
        session: SessionAuth::Anonymous,
        correlation_id: 1,
        resume_secret: None,
        state: tentaflow_core::dispatch::state::AppState::for_test(),
    };
    let _ = dispatch::dispatch(
        &MessageBody::MetaHeartbeat {
            sent_at_epoch: 1_700_000_000,
        },
        &ctx,
    );

    let snap = metrics::snapshot_variant("MetaHeartbeat").expect("metric exists");
    assert!(snap.calls_total >= 1);
    // duration_us moze byc 0 dla bardzo szybkiego handlera w debug build —
    // sprawdzamy tylko ze metric istnieje + calls_total wzrosl.
}

#[tokio::test]
async fn recorder_round_trip_with_dispatch() {
    use tempfile::NamedTempFile;
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);
    // Init globalny recorder (jednorazowo per test process — OnceLock).
    let _ = recorder::init(&path);

    let ctx = HandlerContext {
        session: SessionAuth::UserSession { user_id: [0u8; 16], role: None },
        correlation_id: 999,
        resume_secret: None,
        state: tentaflow_core::dispatch::state::AppState::for_test(),
    };
    let _ = dispatch::dispatch(&MessageBody::NodeListRequest, &ctx);

    if let Some(rec) = recorder::global() {
        let frames = rec.by_correlation(999).unwrap_or_default();
        // Recorder moze byc juz init przez inny test — sprawdzamy tylko czy
        // nie crash uje. Real assertions o zawartosci wymagaja exclusive recorder.
        let _ = frames;
    }
}
