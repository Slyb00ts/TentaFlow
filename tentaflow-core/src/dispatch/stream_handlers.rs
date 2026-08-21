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
use tentaflow_protocol::{
    ChatStreamChunk, ChatStreamEnd, FlowInputValue, FlowInvokeChunk, FlowInvokeEnd, MessageBody,
    SessionAuth,
};
use tokio_util::sync::CancellationToken;

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
    use crate::api::openai::types::{
        ChatCompletionRequest, Message, MessageContent, StreamOptions,
    };

    let stream_req = match req {
        MessageBody::ChatStreamRequestBody(r) => r,
        _ => {
            let _ = push_end(
                &sub,
                Some(MessageBody::ChatStreamEndBody(ChatStreamEnd {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    text: None,
                    ttft_ms: 0,
                    prefill_tps: 0.0,
                    decode_tps: 0.0,
                    total_ms: 0,
                })),
            );
            return;
        }
    };

    let router = ctx.state.router.clone();
    tokio::spawn(async move {
        // Diagnostic anchor: what the dashboard ACTUALLY sent. flow_id=None +
        // a non-empty model_id means the browser-side flow selector returned
        // empty — every "why did chat use a random local model" hunt starts
        // by reading this line, not by inspecting the DOM.
        tracing::info!(
            model_id = %stream_req.model_id,
            flow_id = ?stream_req.flow_id,
            "chat stream request received"
        );
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
            reasoning_effort: None,
            modalities: None,
            audio: None,
            model: stream_req.model_id.clone(),
            messages,
            temperature: stream_req.temperature,
            max_tokens: stream_req.max_tokens,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            stream: true,
            // Dashboard chce metryk per-message (TTFT, prefill/decode tok/s) i
            // realnych liczb tokenow — wlaczamy usage-tail, ktory niesie usage
            // i perf na finalnym chunku.
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            user: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            n: None,
            // The dashboard conversation id IS the flow session id — thread it
            // so `conversation_history` / `memory` nodes in the selected flow
            // have a session key (without it Agent-style flows hard-fail with
            // "no session_id"). memory_options is the only carrier the flow
            // builder reads (build_initial_envelope_inner).
            memory_options: stream_req.session_id.clone().map(|sid| {
                crate::api::openai::types::MemoryOptions {
                    session_id: Some(sid),
                    ..Default::default()
                }
            }),
            audio_input: None,
            extra: Default::default(),
        };

        // Selektor flow z UI czatu: konkretny flow po ID albo "Default Chat".
        // "Default Chat" = Auto: czysty model / alias→model wykonywany wprost na
        // backendzie (bez flow, bez pii). Tylko flow published as a model /
        // alias→flow trafia w flow engine.
        let flow_selector = match stream_req.flow_id.clone() {
            Some(flow_id) => crate::routing::streaming::ChatFlowSelector::FlowId(flow_id),
            None => crate::routing::streaming::ChatFlowSelector::Auto,
        };
        // Realny zalogowany użytkownik z sesji → atrybucja zużycia tokenów i kwot
        // per-user (bez tego AiGateway zapisywałby zużycie na sentinel __system__).
        let user = match &ctx.session {
            SessionAuth::UserSession { user_id, role } => Some(crate::auth::acl::UserContext::new(
                super::handlers::user_id_to_uuid(user_id),
                role.clone().unwrap_or_else(|| "user".to_string()),
            )),
            _ => None,
        };
        // §2.5 — dashboard chat surface; the actor is the session user.
        let actor = match user.as_ref() {
            Some(u) => crate::flow_engine::dispatcher::FlowActor::user(u.user_id.clone()),
            None => crate::flow_engine::dispatcher::FlowActor::system(),
        };
        let route_result = match router
            .route_chat_completion_stream(
                request,
                user,
                crate::flow_engine::dispatcher::FlowOrigin::Chat,
                actor,
                None,
                flow_selector,
            )
            .await
        {
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
                        text: None,
                        ttft_ms: 0,
                        prefill_tps: 0.0,
                        decode_tps: 0.0,
                        total_ms: 0,
                    })),
                );
                return;
            }
        };

        let mut stream = route_result.response;
        let mut completion_tokens: u32 = 0;
        // Realne liczniki i metryki z finalnego chunku (usage-tail). Gdy backend
        // ich nie zaraportuje, zostaja przy domyslnych zerach / liczeniu delt.
        let mut prompt_tokens: u32 = 0;
        let mut final_completion_tokens: Option<u32> = None;
        let mut perf: Option<crate::api::openai::types::GenPerf> = None;
        // Suma wszystkich wyslanych delt — ChatStreamEnd.text pozwala
        // frontendowi odtworzyc odpowiedz gdy zlozone delty sa puste.
        let mut full_text = String::new();
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
                    let payload = format!("\n[stream error] {}", e);
                    full_text.push_str(&payload);
                    let _ = push_chunk_async(
                        &sub,
                        MessageBody::ChatStreamChunkBody(ChatStreamChunk { delta: payload }),
                    )
                    .await;
                    break;
                }
            };
            // Usage-tail (choices: [], usage+perf) — realne liczby z silnika.
            if let Some(u) = chunk.usage.as_ref() {
                prompt_tokens = u.prompt_tokens;
                final_completion_tokens = Some(u.completion_tokens);
            }
            if let Some(p) = chunk.perf {
                perf = Some(p);
            }
            if let Some(choice) = chunk.choices.first() {
                let reasoning = choice
                    .delta
                    .reasoning_content
                    .as_deref()
                    .filter(|s| !s.is_empty());
                let content = choice.delta.content.as_deref().filter(|s| !s.is_empty());

                if let Some(r) = reasoning {
                    let payload = if in_thinking {
                        r.to_string()
                    } else {
                        in_thinking = true;
                        format!("<think>{}", r)
                    };
                    full_text.push_str(&payload);
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
                    full_text.push_str(&payload);
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
            full_text.push_str("</think>");
            let _ = push_chunk_async(
                &sub,
                MessageBody::ChatStreamChunkBody(ChatStreamChunk {
                    delta: "</think>".to_string(),
                }),
            )
            .await;
        }

        let perf = perf.unwrap_or_default();
        let _ = push_end_async(
            &sub,
            Some(MessageBody::ChatStreamEndBody(ChatStreamEnd {
                prompt_tokens,
                // Realny licznik z usage-tail gdy backend go dostarczyl; w innym
                // wypadku liczba wyslanych delt jako przyblizenie.
                completion_tokens: final_completion_tokens.unwrap_or(completion_tokens),
                text: if full_text.is_empty() {
                    None
                } else {
                    Some(full_text)
                },
                ttft_ms: perf.ttft_ms,
                prefill_tps: perf.prefill_tps,
                decode_tps: perf.decode_tps,
                total_ms: perf.total_ms,
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
// FlowInvokeRequest — uniwersalny multimodalny most do flow engine. Zapisuje
// bajty wejść do blob store, buduje FlowEnvelope, woła FlowDispatcher::
// try_dispatch_streaming i mapuje EnvelopeDelta → FlowInvokeChunk. Zastępuje
// REST /v1/audio/* dla powierzchni dashboardu (chat audio).
// =============================================================================

fn flow_invoke_handler(req: MessageBody, ctx: HandlerContext, sub: Arc<Subscription>) {
    use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin, FlowRequestMeta};
    use crate::flow_engine::envelope::EnvelopeDelta;

    let invoke = match req {
        MessageBody::FlowInvokeRequestBody(r) => r,
        _ => {
            let _ = push_end(
                &sub,
                Some(MessageBody::FlowInvokeEndBody(FlowInvokeEnd {
                    finish_reason: "error".into(),
                    error: Some("bad request body".into()),
                    text: None,
                })),
            );
            return;
        }
    };

    let router = ctx.state.router.clone();
    let progress_broker = ctx.state.progress_broker.clone();
    // Authenticated principal of this foreground flow. Bound to the session
    // scope below so run-events ACL (§3.3) can reject a foreign subscriber — the
    // client-minted session id is not an authorization token on its own.
    let actor_id = match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            Some(uuid::Uuid::from_bytes(*user_id).hyphenated().to_string())
        }
        _ => None,
    };

    // Rozwiazanie jezyka TTS per-request, w kolejnosci priorytetow:
    //  1. jawny `language` z requestu klienta,
    //  2. preferowany jezyk zalogowanego usera (ustawienie programu),
    //  3. (gdy oba puste) jezyk wykryty przez STT w samym flow,
    //  4. fallback po stronie silnika TTS.
    // Kroki 1-2 rozstrzygamy tutaj. Gdy oba sa puste zostawiamy `None`, by nie
    // nadpisac jezyka wykrytego przez STT (krok 3) sztywnym fallbackiem.
    let resolved_language = match invoke.language.clone() {
        Some(lang) => Some(lang),
        None => match &ctx.session {
            SessionAuth::UserSession { user_id, .. } => {
                let uid = super::handlers::user_id_to_uuid(user_id);
                crate::db::repository::get_user_preferred_language(&ctx.state.db, &uid)
                    .ok()
                    .flatten()
            }
            _ => None,
        },
    };

    tokio::spawn(async move {
        let Some(fd) = router.flow_dispatcher().cloned() else {
            let _ = push_end(
                &sub,
                Some(MessageBody::FlowInvokeEndBody(FlowInvokeEnd {
                    finish_reason: "error".into(),
                    error: Some("flow dispatcher unavailable".into()),
                    text: None,
                })),
            );
            return;
        };

        let blobs = fd.blobs();
        let mut envelope = match flow_envelope_from_inputs(
            invoke.inputs,
            resolved_language,
            invoke.output_audio,
            invoke.stt_model,
            invoke.tts_model,
            &blobs,
        )
        .await
        {
                Ok(e) => e,
                Err(e) => {
                    let _ = push_end(
                        &sub,
                        Some(MessageBody::FlowInvokeEndBody(FlowInvokeEnd {
                            finish_reason: "error".into(),
                            error: Some(format!("input build failed: {e}")),
                            text: None,
                        })),
                    );
                    return;
                }
            };

        // Cancel wiązany z subskrypcją: rozłączenie klienta / barge-in anuluje flow.
        let cancel = CancellationToken::new();
        // §2.5 — the dashboard chat surface. The actor is the session principal
        // resolved above; an unauthenticated subscription has none, and stamping
        // `system` for it is the honest answer (it can only reach flows the
        // dispatcher's `user_id = None` path allows).
        let actor = match actor_id.as_deref() {
            Some(uid) => FlowActor::user(uid),
            None => FlowActor::system(),
        };
        // §2.5 / §3 invariant 1 — the correlation key is MINTED HERE, after
        // authorization, and is unique per run. `ctx.correlation_id` is the
        // client-supplied per-connection frame id: two connections both at frame
        // 7 would collide, and a client picks it, so it can never be the key the
        // audit trail joins on.
        let correlation_key = uuid::Uuid::new_v4().to_string();
        let mut meta = FlowRequestMeta::new(
            format!("flowinvoke-{correlation_key}"),
            FlowOrigin::Chat,
            actor,
        );
        meta.session_id = invoke.session_id.clone();
        meta.user_id = actor_id.clone();
        // Joinable with `request_id` — both are built from the minted key, so
        // the audit trail and the run share it.
        meta.correlation_id = Some(correlation_key.clone());
        // Same value on the envelope, the way `routing/chat.rs` seeds it: the
        // per-call `llm` audit rows and the agent/vision nodes read it from meta.
        // The struct field above stays the authority — a node that rewrites meta
        // cannot move the run's audit link.
        envelope.meta.insert(
            "correlation_id".into(),
            serde_json::Value::String(correlation_key),
        );
        meta.cancel_token = cancel.clone();

        // Bind the session scope to this principal so run-events ACL (§3.3) can
        // authorize a `Session` subscription — the progress scope is this
        // session id (engine plumbing), and without the binding any user could
        // subscribe by guessing it.
        if let (Some(session_id), Some(actor)) = (invoke.session_id.as_deref(), actor_id.as_deref())
        {
            progress_broker.bind_session_owner(session_id, actor);
        }

        // flow_id ma priorytet — odpala dokładnie ten flow który user wybrał
        // (np. w trybie audio). Bez niego rozwiązanie przez model/service_type.
        let dispatch = if let Some(fid) = invoke.flow_id {
            fd.dispatch_by_flow_id_streaming(fid, envelope, meta).await
        } else {
            fd.try_dispatch_streaming(&invoke.model, &invoke.service_type, envelope, meta)
                .await
        };
        let exec = match dispatch {
            Ok(e) => e,
            Err(e) => {
                let _ = push_end(
                    &sub,
                    Some(MessageBody::FlowInvokeEndBody(FlowInvokeEnd {
                        finish_reason: "error".into(),
                        error: Some(format!("dispatch failed: {e}")),
                        text: None,
                    })),
                );
                return;
            }
        };

        // Pre-producer nodes (stt) are done by now; surface the transcript
        // once so the client can render the user's utterance immediately.
        if let Some(text) = exec
            .producer_input
            .meta
            .get("stt_transcript")
            .and_then(|v| v.as_str())
            .filter(|t| !t.is_empty())
        {
            if push_chunk_async(
                &sub,
                MessageBody::FlowInvokeChunkBody(FlowInvokeChunk::Transcript {
                    text: text.to_string(),
                }),
            )
            .await
            .is_err()
            {
                cancel.cancel();
                return;
            }
        }

        let mut stream = exec.stream;
        // Akumulujemy pelny tekst po stronie serwera (autorytatywne zrodlo) — delty
        // streamu moga zostac uciete u klienta gdy audio leci dluzej niz tekst.
        let mut full_text = String::new();
        while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(EnvelopeDelta::Llm(c)) => {
                    if c.text_delta.is_empty() {
                        continue;
                    }
                    full_text.push_str(&c.text_delta);
                    FlowInvokeChunk::Text {
                        choice_index: c.choice_index,
                        delta: c.text_delta,
                    }
                }
                Ok(EnvelopeDelta::Audio(a)) => {
                    if a.bytes_delta.is_empty() {
                        continue;
                    }
                    FlowInvokeChunk::Audio {
                        choice_index: a.choice_index,
                        mime: a.mime,
                        sample_rate: a.sample_rate,
                        bytes: a.bytes_delta,
                    }
                }
                Err(e) => {
                    let _ = push_end_async(
                        &sub,
                        Some(MessageBody::FlowInvokeEndBody(FlowInvokeEnd {
                            finish_reason: "error".into(),
                            error: Some(format!("stream error: {e}")),
                            text: None,
                        })),
                    )
                    .await;
                    cancel.cancel();
                    return;
                }
            };
            if push_chunk_async(&sub, MessageBody::FlowInvokeChunkBody(chunk))
                .await
                .is_err()
            {
                cancel.cancel();
                return;
            }
        }

        let (finish_reason, error) = match exec.outcome.await {
            Ok(outcome) => (
                format!("{:?}", outcome.finish_reason).to_lowercase(),
                outcome.error,
            ),
            Err(_) => ("stop".into(), None),
        };
        let _ = push_end_async(
            &sub,
            Some(MessageBody::FlowInvokeEndBody(FlowInvokeEnd {
                finish_reason,
                error,
                text: Some(full_text),
            })),
        )
        .await;
    });
}

/// Buduje FlowEnvelope z multimodalnych wejść: pierwsze → payload, kolejne →
/// artefakty `input_{n}`. Bajty media trafiają do blob store jako BlobRef.
async fn flow_envelope_from_inputs(
    inputs: Vec<FlowInputValue>,
    language: Option<String>,
    output_audio: bool,
    stt_model: Option<String>,
    tts_model: Option<String>,
    blobs: &Arc<dyn crate::flow_engine::blob_store::BlobStore>,
) -> anyhow::Result<crate::flow_engine::envelope::FlowEnvelope> {
    use crate::flow_engine::envelope::{ArtifactProvenance, FlowEnvelope, FlowValue};

    let mut env: Option<FlowEnvelope> = None;
    let mut artifact_idx = 0usize;
    for input in inputs {
        let value = match input {
            FlowInputValue::Text(t) => FlowValue::Text(t),
            FlowInputValue::Json(j) => {
                FlowValue::Json(serde_json::from_str(&j).unwrap_or(serde_json::Value::String(j)))
            }
            FlowInputValue::Audio {
                mime,
                sample_rate,
                bytes,
            } => {
                let r = blobs.put(bytes, &mime).await?;
                FlowValue::Audio {
                    blob_ref: r,
                    mime,
                    sample_rate,
                }
            }
            FlowInputValue::Image { mime, bytes } => {
                let r = blobs.put(bytes, &mime).await?;
                FlowValue::Image {
                    blob_ref: r,
                    mime,
                    dims: None,
                }
            }
            FlowInputValue::Video { mime, bytes } => {
                let r = blobs.put(bytes, &mime).await?;
                FlowValue::Video {
                    blob_ref: r,
                    mime,
                    duration_ms: None,
                }
            }
            FlowInputValue::File {
                mime,
                filename,
                bytes,
            } => {
                let r = blobs.put(bytes, &mime).await?;
                FlowValue::Other {
                    blob_ref: r,
                    mime,
                    filename,
                }
            }
        };
        match env {
            None => env = Some(FlowEnvelope::with_payload(value)),
            Some(ref mut e) => {
                let _ = e.put_artifact(
                    format!("input_{artifact_idx}"),
                    value,
                    ArtifactProvenance {
                        producer_node_id: "flow_invoke".into(),
                        producer_node_type: "transport".into(),
                        timestamp_ms: 0,
                    },
                );
                artifact_idx += 1;
            }
        }
    }
    let mut env = env.unwrap_or_else(FlowEnvelope::empty);
    if let Some(lang) = language {
        env.meta
            .insert("language".into(), serde_json::Value::String(lang));
    }
    env.set_output_audio(output_audio);
    if let Some(m) = stt_model.filter(|m| !m.is_empty()) {
        env.meta
            .insert("stt_model".into(), serde_json::Value::String(m));
    }
    if let Some(m) = tts_model.filter(|m| !m.is_empty()) {
        env.meta
            .insert("tts_model".into(), serde_json::Value::String(m));
    }
    Ok(env)
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "FlowInvokeRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: flow_invoke_handler,
    }
}

// =============================================================================
// ClusterProbeStreamRequest — streaming probe miedzy nodami klastra.
// Wysyla "started" → seria "probing_pair"/"result" → "complete" + End z agregatami.
// =============================================================================

/// Maska advertowana przez peera nie zawsze jest poprawnym IPv4 (czasem pusta).
/// Gdy nie da sie jej sparsowac, zakladamy /24 — realna osiagalnosc jest i tak
/// potwierdzana udanym connectem podczas probe.
fn netmask_or_24(advertised: &str) -> String {
    if advertised.parse::<std::net::Ipv4Addr>().is_ok() {
        advertised.to_string()
    } else {
        "255.255.255.0".to_string()
    }
}

/// Buduje liste interfejsow noda dla orkiestracji probe z advertowanych sieci.
/// Bierze tylko karty UP z adresem IPv4 — tylko po nich da sie probowac.
fn node_interfaces_from_networks(
    node_id: &str,
    nets: &[crate::mesh::peer_store::PeerNetworkInfo],
) -> Vec<crate::mesh::cluster_probe::NodeInterface> {
    nets.iter()
        .filter(|n| n.link_up && !n.ipv4_address.is_empty())
        .map(|n| crate::mesh::cluster_probe::NodeInterface {
            node_id: node_id.to_string(),
            name: n.name.clone(),
            ip: n.ipv4_address.clone(),
            netmask: netmask_or_24(&n.ipv4_netmask),
            speed_mbps: n.speed_mbps.unwrap_or(0),
            rdma_available: n.rdma_available,
        })
        .collect()
}

/// Uruchamia jeden pomiar przepustowosci miedzy konkretna para interfejsow.
/// `server` nasluchuje (bind do swojej karty), `client` laczy sie do IP servera
/// bindujac do swojej karty. Nody zdalne sterowane sa przez mesh command;
/// lokalny node wola silnik probe wprost (bez wysylania komendy do samego siebie).
/// Zwraca `(bandwidth_mbps, latency_us, rdma)` przy sukcesie.
async fn run_interface_probe(
    qm: &Option<Arc<crate::mesh::iroh_manager::IrohMeshManager>>,
    local_id: &str,
    server: &crate::mesh::cluster_probe::NodeInterface,
    client: &crate::mesh::cluster_probe::NodeInterface,
    nonce: &[u8; 32],
    num_streams: u8,
    duration_ms: u32,
) -> Option<(f64, u64, bool)> {
    use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType};

    const CMD_TIMEOUT_SECS: u64 = 45;

    // 1. Strona serwera — startuje listener, zwraca przydzielony port TCP.
    let (tcp_port, rdma_port) = if server.node_id == local_id {
        match crate::mesh::bandwidth_probe::start_probe_server(
            &server.ip,
            nonce,
            num_streams,
            duration_ms,
        )
        .await
        {
            Ok((port, handle)) => {
                tokio::spawn(async move {
                    let _ = handle.await;
                });
                (port, 0u16)
            }
            Err(e) => {
                tracing::warn!("local probe server na {} nie wstal: {}", server.ip, e);
                return None;
            }
        }
    } else {
        let qm = qm.as_ref()?;
        let cmd = MeshCommandType::BandwidthProbe {
            target_ip: server.ip.clone(),
            target_port: 0,
            rdma_port: 0,
            bind_interface: server.name.clone(),
            duration_ms,
            mode: "server".into(),
            nonce: nonce.to_vec(),
            num_streams,
        };
        match qm
            .send_command_and_wait(&server.node_id, cmd, CMD_TIMEOUT_SECS)
            .await
        {
            Ok(resp) if resp.ok => match resp.payload {
                MeshCommandResponsePayload::BandwidthProbeServerStarted {
                    tcp_port,
                    rdma_port,
                } => (tcp_port, rdma_port),
                _ => {
                    tracing::warn!("probe server: nieoczekiwany payload");
                    return None;
                }
            },
            Ok(resp) => {
                tracing::warn!("probe server blad: {:?}", resp.error);
                return None;
            }
            Err(e) => {
                tracing::warn!("probe server send nieudany: {}", e);
                return None;
            }
        }
    };

    // 2. Strona klienta — laczy sie do servera, mierzy i zwraca metryki.
    // `start_probe_client` zwraca Ok nawet gdy ZADEN data-stream sie nie polaczyl
    // (np. brak trasy do `server.ip` mimo wspolnego /24): bandwidth=0,
    // streams_completed=0. Taki "sukces" to faktyczny brak osiagalnosci — odrzucamy
    // go juz tutaj (None), zeby falszywy wynik nie wygral pary ani nie liczyl sie
    // jako reachable.
    let client_result: Option<(f64, u64, bool)> = if client.node_id == local_id {
        match crate::mesh::bandwidth_probe::start_probe_client(
            &server.ip,
            tcp_port,
            &client.name,
            nonce,
            num_streams,
            duration_ms,
        )
        .await
        {
            Ok(r) if r.streams_completed > 0 && r.bandwidth_mbps > 0.0 => {
                Some((r.bandwidth_mbps, r.latency_us, false))
            }
            Ok(r) => {
                tracing::warn!(
                    "probe {} → {} bez przeplywu (streams={}, mbps={})",
                    client.name,
                    server.ip,
                    r.streams_completed,
                    r.bandwidth_mbps
                );
                None
            }
            Err(e) => {
                tracing::warn!("local probe client do {} nieudany: {}", server.ip, e);
                None
            }
        }
    } else {
        match qm.as_ref() {
            Some(qm) => {
                let cmd = MeshCommandType::BandwidthProbe {
                    target_ip: server.ip.clone(),
                    target_port: tcp_port,
                    rdma_port,
                    bind_interface: client.name.clone(),
                    duration_ms,
                    mode: "client".into(),
                    nonce: nonce.to_vec(),
                    num_streams,
                };
                match qm
                    .send_command_and_wait(&client.node_id, cmd, CMD_TIMEOUT_SECS)
                    .await
                {
                    Ok(resp) if resp.ok => match resp.payload {
                        MeshCommandResponsePayload::BandwidthProbeClientResult {
                            bandwidth_mbps,
                            latency_us,
                            rdma,
                            streams_completed,
                            ..
                        } if streams_completed > 0 && bandwidth_mbps > 0.0 => {
                            Some((bandwidth_mbps, latency_us, rdma))
                        }
                        MeshCommandResponsePayload::BandwidthProbeClientResult {
                            bandwidth_mbps,
                            streams_completed,
                            ..
                        } => {
                            tracing::warn!(
                                "probe {} → {} bez przeplywu (streams={}, mbps={})",
                                client.name,
                                server.ip,
                                streams_completed,
                                bandwidth_mbps
                            );
                            None
                        }
                        _ => {
                            tracing::warn!("probe client: nieoczekiwany payload");
                            None
                        }
                    },
                    Ok(resp) => {
                        tracing::warn!("probe client blad: {:?}", resp.error);
                        None
                    }
                    Err(e) => {
                        tracing::warn!("probe client send nieudany: {}", e);
                        None
                    }
                }
            }
            None => None,
        }
    };

    // Gdy klient padl, zdalny serwer probe i tak sam zwalnia listener po wlasnym
    // SERVER_TIMEOUT (~30s) — `BandwidthProbeCancel` w executorze jest no-opem,
    // wiec swiadomie go nie wysylamy, by nie generowac bezuzytecznego round-tripu.
    client_result
}

fn cluster_probe_stream_handler(req: MessageBody, ctx: HandlerContext, sub: Arc<Subscription>) {
    use crate::db::repository;
    use crate::mesh::cluster_probe;
    use tentaflow_protocol::{
        ClusterProbeAssignment, ClusterProbeStreamChunk, ClusterProbeStreamEnd,
        ClusterProbeStreamRequest,
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
                            bottleneck_mbps: None,
                            assignment_status: None,
                            assignments: Vec::new(),
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
                        bottleneck_mbps: None,
                        assignment_status: None,
                        assignments: Vec::new(),
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

        const NUM_STREAMS: u8 = 4;
        const DURATION_MS: u32 = 2000;

        // Zbuduj liste interfejsow per node (rownolegla do payload.node_ids).
        // Lokalny node czyta swoje karty wprost; zdalne z peer_store (advertowane
        // przez heartbeat). Netmaska nie zawsze jest w advertise → /24 heurystyka.
        let node_ifaces: Vec<Vec<cluster_probe::NodeInterface>> = payload
            .node_ids
            .iter()
            .map(|nid| {
                let nets = if *nid == local_id {
                    crate::mesh::node_info_collector::collect_fast_metrics().networks
                } else {
                    ctx.state
                        .mesh_peer_store
                        .get(nid)
                        .map(|p| p.networks)
                        .unwrap_or_default()
                };
                node_interfaces_from_networks(nid, &nets)
            })
            .collect();

        // Pary interfejsow w tym samym subnecie, pogrupowane per para nodow i
        // posortowane od najszybszego (wg sysfs speed). Probujemy WSZYSTKIE
        // kandydatury per para, zeby zmierzyc np. szybki ConnectX 10.10.10 obok
        // wolniejszego LAN-u 192.168.x i wybrac realnie najszybszy link.
        let reachable = cluster_probe::filter_reachable_pairs(&node_ifaces);
        let ranked = cluster_probe::rank_pairs_by_speed(&reachable);

        let mut all_results: Vec<cluster_probe::PairProbeResult> = Vec::new();
        let mut total_pairs: u32 = 0;
        let mut successful: u32 = 0;
        let mut failed: u32 = 0;

        for i in 0..payload.node_ids.len() {
            for j in (i + 1)..payload.node_ids.len() {
                let a = payload.node_ids[i].clone();
                let b = payload.node_ids[j].clone();
                total_pairs += 1;

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

                let key = if a < b {
                    (a.clone(), b.clone())
                } else {
                    (b.clone(), a.clone())
                };
                let candidates = ranked.get(&key).cloned().unwrap_or_default();

                // Zmierz kazda kandydujaca pare interfejsow, zachowaj najlepsza.
                // Do puli wynikow trafiaja TYLKO realnie osiagalne pomiary —
                // `run_interface_probe` zwraca None dla no-op (zero strumieni /
                // zero przeplywu), wiec falszywe "sukcesy" nigdy nie wygraja pary.
                let mut best: Option<cluster_probe::PairProbeResult> = None;
                for (x, y) in &candidates {
                    let nonce: [u8; 32] = rand::random();
                    let Some((bw, lat_us, rdma)) =
                        run_interface_probe(&qm, &local_id, x, y, &nonce, NUM_STREAMS, DURATION_MS)
                            .await
                    else {
                        continue;
                    };

                    // Zmapuj interfejsy x/y na strony a/b po node_id.
                    let (interface_a, interface_b) = if x.node_id == a {
                        (x.name.clone(), y.name.clone())
                    } else {
                        (y.name.clone(), x.name.clone())
                    };

                    let result = cluster_probe::PairProbeResult {
                        node_a: a.clone(),
                        node_b: b.clone(),
                        interface_a,
                        interface_b,
                        bandwidth_mbps: bw,
                        latency_us: lat_us,
                        reachable: true,
                        rdma,
                    };
                    all_results.push(result.clone());

                    match &best {
                        Some(p) if p.bandwidth_mbps >= result.bandwidth_mbps => {}
                        _ => best = Some(result),
                    }
                }

                // Result chunk z wygrywajacym interfejsem. Gdy para nie ma ZADNEGO
                // osiagalnego linku (brak kandydatow w tym samym subnecie ALBO
                // wszystkie probe padly), wpisz jawny nieosiagalny wynik do puli —
                // inaczej `optimal_assignment` nie wie o tej parze i raportuje
                // "optimal" mimo dziury w topologii.
                let (success, latency_ms, bandwidth_mbps, interface_type, message) = match &best {
                    Some(bp) => {
                        successful += 1;
                        let lat_ms = ((bp.latency_us as f64) / 1000.0).round() as u32;
                        let kind = if bp.rdma { "rdma" } else { "ethernet" };
                        (
                            true,
                            Some(lat_ms),
                            Some(bp.bandwidth_mbps.round() as u32),
                            Some(bp.interface_a.clone()),
                            Some(format!(
                                "{} ↔ {} ({})",
                                bp.interface_a, bp.interface_b, kind
                            )),
                        )
                    }
                    None => {
                        failed += 1;
                        all_results.push(cluster_probe::PairProbeResult {
                            node_a: a.clone(),
                            node_b: b.clone(),
                            interface_a: String::new(),
                            interface_b: String::new(),
                            bandwidth_mbps: 0.0,
                            latency_us: 0,
                            reachable: false,
                            rdma: false,
                        });
                        (false, None, None, None, None)
                    }
                };

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
                        message,
                    }),
                )
                .is_err()
                {
                    return;
                }
            }
        }

        // Optymalne przypisanie: per para najszybszy link + per-node wybrany NIC.
        let detection = cluster_probe::optimal_assignment(&all_results);

        // Cluster_id: z requestu (gdy frontend go poda) albo wyprowadzony z DB po
        // zbiorze czlonkow rownym node_ids. Bez niego nie persistujemy interfejsu.
        let cluster_id = payload.cluster_id.clone().or_else(|| {
            repository::find_cluster_by_member_set(&ctx.state.db, &payload.node_ids)
                .ok()
                .flatten()
        });

        let find_iface = |node_id: &str, name: &str| {
            node_ifaces
                .iter()
                .flatten()
                .find(|nif| nif.node_id == node_id && nif.name == name)
        };

        let mut assignments: Vec<ClusterProbeAssignment> = Vec::new();
        for (node_id, na) in &detection.per_node {
            let (ip, speed, itype) = match find_iface(node_id, &na.interface) {
                Some(nif) => (
                    nif.ip.clone(),
                    nif.speed_mbps as u32,
                    if nif.rdma_available {
                        "rdma"
                    } else {
                        "ethernet"
                    }
                    .to_string(),
                ),
                None => (String::new(), 0u32, "ethernet".to_string()),
            };

            if let Some(cid) = &cluster_id {
                if let Err(e) = repository::update_cluster_member_interface(
                    &ctx.state.db,
                    cid,
                    node_id,
                    &na.interface,
                    &ip,
                    speed as i64,
                    &itype,
                ) {
                    tracing::warn!("persist cluster member iface ({}): {}", node_id, e);
                }
            }

            assignments.push(ClusterProbeAssignment {
                node_id: node_id.clone(),
                interface_name: na.interface.clone(),
                interface_ip: ip,
                interface_speed_mbps: speed,
                interface_type: itype,
            });
        }

        let bottleneck_mbps = if detection.bottleneck_mbps > 0.0 {
            Some(detection.bottleneck_mbps.round() as u32)
        } else {
            None
        };

        // Complete chunk + End z agregatami i wybranym przypisaniem.
        let _ = push_chunk(
            &sub,
            MessageBody::ClusterProbeStreamChunkBody(ClusterProbeStreamChunk {
                event_type: "complete".into(),
                source_node: None,
                target_node: None,
                success: None,
                latency_ms: None,
                bandwidth_mbps: bottleneck_mbps,
                interface_type: Some(detection.message.clone()),
                message: Some(format!(
                    "{} pairs, {} ok, bottleneck {} Mbps",
                    total_pairs,
                    successful,
                    bottleneck_mbps.unwrap_or(0)
                )),
            }),
        );

        let _ = push_end(
            &sub,
            Some(MessageBody::ClusterProbeStreamEndBody(
                ClusterProbeStreamEnd {
                    total_pairs,
                    successful,
                    failed,
                    bottleneck_mbps,
                    assignment_status: Some(detection.message),
                    assignments,
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
                            tentaflow_protocol::cbor::decode::<MessageBody>(&frame.body_bytes)
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
                    crate::services_repo::deployments::DeploymentStatus::Failed => "failed",
                    crate::services_repo::deployments::DeploymentStatus::Cancelled => "cancelled",
                    crate::services_repo::deployments::DeploymentStatus::Interrupted => {
                        "interrupted"
                    }
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
// BenchmarkRunStream — live progres runu Benchmark Studio.
// =============================================================================
// Front subskrybuje przez ApiBinary.subscribe('benchmarkRunStreamRequest',
// { runId }). Reużywamy tę samą szynę (log_bus) co deployment: StartRun emituje
// BusMessage::Line/End pod kluczem = run_id. Handler mapuje je na
// BenchmarkBody(RunStreamChunk/RunStreamEnd). Brak replayu z DB — postęp jest
// best-effort live; ostateczne wyniki front pobiera przez RunResults/RunStatus.

fn benchmark_run_stream_handler(req: MessageBody, ctx: HandlerContext, sub: Arc<Subscription>) {
    use tentaflow_protocol::BenchmarkPayload;

    let run_id = match req {
        MessageBody::BenchmarkBody(BenchmarkPayload::RunStreamRequest { run_id }) => run_id,
        _ => {
            let _ = push_end(&sub, None);
            return;
        }
    };

    // Autoryzacja: subskrybent musi mieć benchmark.read w swojej org, a run musi
    // należeć do tej org (IDOR guard — inaczej każdy zalogowany user znający run_id
    // mógłby podglądać cudze logi/postęp).
    let org_id = match ctx.org_context.as_ref() {
        Some(org) if org.has("benchmark.read") => org.org_id.clone(),
        _ => {
            let _ = push_end(&sub, None);
            return;
        }
    };
    match crate::db::repository::get_benchmark_run(&ctx.state.db, &org_id, &run_id) {
        Ok(Some(_)) => {}
        _ => {
            // Brak runu w tej org (nie istnieje lub należy do innej org) → odmowa.
            let _ = push_end(&sub, None);
            return;
        }
    }

    tokio::spawn(async move {
        let mut rx = match crate::deploy::log_bus::subscribe(&run_id) {
            Some(r) => r,
            None => {
                // Kanał zamknięty — run już skończony (albo nie istnieje). Front
                // rekoncyliuje stan przez RunStatus/RunResults.
                let _ = push_end(&sub, None);
                return;
            }
        };

        use crate::deploy::log_bus::BusMessage;
        loop {
            match rx.recv().await {
                Ok(BusMessage::Line(line)) => {
                    let chunk = BenchmarkPayload::RunStreamChunk {
                        run_id: line.deploy_id,
                        kind: line.kind,
                        phase: line.phase,
                        line: line.line,
                        progress_pct: line.progress_pct,
                        ts_ms: line.ts_ms,
                    };
                    if push_chunk(&sub, MessageBody::BenchmarkBody(chunk)).is_err() {
                        return;
                    }
                }
                Ok(BusMessage::End {
                    deploy_id,
                    final_status,
                    error_message,
                    ..
                }) => {
                    let error = if error_message.is_empty() {
                        None
                    } else {
                        Some(error_message)
                    };
                    let end = BenchmarkPayload::RunStreamEnd {
                        run_id: deploy_id,
                        status: final_status,
                        error,
                    };
                    let _ = push_end(&sub, Some(MessageBody::BenchmarkBody(end)));
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    let _ = push_end(&sub, None);
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "BenchmarkRunStreamRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: benchmark_run_stream_handler,
    }
}

// =============================================================================
// ProjectStudioIngestStream — live progress of a Project Studio ingest job.
// =============================================================================
// The frontend subscribes via ApiBinary.subscribe('projectStudioIngestStreamRequest',
// { projectId, jobId }). log_bus is reused: ingest::start_job emits
// BusMessage::Line/End under key = job_id; the handler maps them to
// ProjectStudioBody(IngestStreamChunk/IngestStreamEnd). Polling
// IngestStatusRequest remains the source of truth for job state.

fn project_studio_ingest_stream_handler(
    req: MessageBody,
    ctx: HandlerContext,
    sub: Arc<Subscription>,
) {
    use tentaflow_protocol::project_studio::ProjectStudioPayload;

    let (project_id, job_id) = match req {
        MessageBody::ProjectStudioBody(ProjectStudioPayload::IngestStreamRequest {
            project_id,
            job_id,
        }) => (project_id, job_id),
        _ => {
            let _ = push_end(&sub, None);
            return;
        }
    };

    // IDOR guard: project_studio.read + project in the subscriber's org +
    // real membership (or project_studio.admin — inspection outside
    // membership) + the job MUST belong to this project (job_id is a
    // process-global log_bus key; without this check any logged-in user who
    // knows a job_id could watch someone else's ingest).
    let Some(org) = ctx.org_context.as_ref() else {
        let _ = push_end(&sub, None);
        return;
    };
    if !org.has("project_studio.read") {
        let _ = push_end(&sub, None);
        return;
    }
    match crate::project_studio::repository::get_project(&org.org_id, &project_id) {
        Ok(Some(_)) => {}
        _ => {
            let _ = push_end(&sub, None);
            return;
        }
    }
    let is_member = matches!(
        crate::project_studio::repository::member_role(&project_id, &org.user_id),
        Ok(Some(_))
    );
    if !is_member && !org.has("project_studio.admin") {
        let _ = push_end(&sub, None);
        return;
    }
    let job_belongs = crate::project_studio::project_db::open(&project_id)
        .ok()
        .and_then(|pool| {
            crate::project_studio::repository::get_ingest_job(&pool, &job_id)
                .ok()
                .flatten()
        })
        .is_some();
    if !job_belongs {
        let _ = push_end(&sub, None);
        return;
    }

    tokio::spawn(async move {
        let mut rx = match crate::deploy::log_bus::subscribe(&job_id) {
            Some(r) => r,
            None => {
                // Channel closed — the job already finished. The frontend
                // reconciles state via IngestStatusRequest.
                let _ = push_end(&sub, None);
                return;
            }
        };

        use crate::deploy::log_bus::BusMessage;
        loop {
            match rx.recv().await {
                Ok(BusMessage::Line(line)) => {
                    let chunk = ProjectStudioPayload::IngestStreamChunk {
                        job_id: line.deploy_id,
                        kind: line.kind,
                        phase: line.phase,
                        line: line.line,
                        progress_pct: line.progress_pct,
                        ts_ms: line.ts_ms,
                    };
                    if push_chunk(&sub, MessageBody::ProjectStudioBody(chunk)).is_err() {
                        return;
                    }
                }
                Ok(BusMessage::End {
                    deploy_id,
                    final_status,
                    error_message,
                    ..
                }) => {
                    let error = if error_message.is_empty() {
                        None
                    } else {
                        Some(error_message)
                    };
                    let end = ProjectStudioPayload::IngestStreamEnd {
                        job_id: deploy_id,
                        status: final_status,
                        error,
                    };
                    let _ = push_end(&sub, Some(MessageBody::ProjectStudioBody(end)));
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    let _ = push_end(&sub, None);
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "ProjectStudioIngestStreamRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: project_studio_ingest_stream_handler,
    }
}

// =============================================================================
// ProjectStudioArchiveStream — live progress of a project export or import.
// =============================================================================
// The frontend subscribes via ApiBinary.subscribe('projectStudioArchiveStreamRequest',
// { jobId }). Authorization is the job's OWNER, not a project role: an import
// has no project yet (the row appears only once the content is in place), so a
// project-scoped gate could not exist for the whole run. Polling
// ProjectExportStatus / ProjectImportStatus stays the source of truth.

fn project_studio_archive_stream_handler(
    req: MessageBody,
    ctx: HandlerContext,
    sub: Arc<Subscription>,
) {
    use tentaflow_protocol::project_studio::ProjectStudioPayload;

    let MessageBody::ProjectStudioBody(ProjectStudioPayload::ArchiveStreamRequest { job_id }) = req
    else {
        let _ = push_end(&sub, None);
        return;
    };
    let Some(org) = ctx.org_context.as_ref() else {
        let _ = push_end(&sub, None);
        return;
    };
    if !org.has("project_studio.read") {
        let _ = push_end(&sub, None);
        return;
    }
    // A bare job id must never expose progress to an unrelated user.
    let owned = crate::project_studio::archive::job(&job_id)
        .is_some_and(|job| job.owner_user_id == org.user_id);
    if !owned {
        let _ = push_end(&sub, None);
        return;
    }

    tokio::spawn(async move {
        let mut rx = match crate::deploy::log_bus::subscribe(&job_id) {
            Some(rx) => rx,
            None => {
                // Channel closed — the job already finished; the frontend
                // reconciles through the status request.
                let _ = push_end(&sub, None);
                return;
            }
        };
        use crate::deploy::log_bus::BusMessage;
        loop {
            match rx.recv().await {
                Ok(BusMessage::Line(line)) => {
                    let chunk = ProjectStudioPayload::ArchiveStreamChunk {
                        job_id: line.deploy_id,
                        phase: line.phase,
                        line: line.line,
                        progress_pct: line.progress_pct,
                        ts_ms: line.ts_ms,
                    };
                    if push_chunk(&sub, MessageBody::ProjectStudioBody(chunk)).is_err() {
                        return;
                    }
                }
                Ok(BusMessage::End {
                    deploy_id,
                    final_status,
                    error_message,
                    ..
                }) => {
                    let end = ProjectStudioPayload::ArchiveStreamEnd {
                        job_id: deploy_id,
                        status: final_status,
                        error: if error_message.is_empty() {
                            None
                        } else {
                            Some(error_message)
                        },
                    };
                    let _ = push_end(&sub, Some(MessageBody::ProjectStudioBody(end)));
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    let _ = push_end(&sub, None);
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "ProjectStudioArchiveStreamRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: project_studio_archive_stream_handler,
    }
}

// =============================================================================
// ProjectStudioChatStream — project chat turn over the shared RAG shell.
// =============================================================================
// The frontend subscribes via ApiBinary.subscribe('projectStudioChatStreamRequest',
// { projectId, chatId, message }). One turn: persist the user message, run the
// seeded `core:rag-query` shell by id (trigger -> loop -> rag_finalize ->
// conversation_history -> llm -> output) — the SAME graph the RAG addon asks,
// with NO project_knowledge node: retrieval happens inside the loop against the
// vector scope minted below. Forward tokens as ChatStreamChunk{kind:"token"},
// emit ONE ChatStreamChunk{kind:"citations"} from the final envelope's rag_citations,
// persist the assistant reply (content + citations_json) and finish with
// ChatStreamEnd{message_id}. Dropping the subscription cancels the generation
// (push failure fires the flow's cancel token — same contract as FlowInvoke).

/// Chat model for a project turn: the project's 'chat' agent binding
/// (`settings['agents']` in project.db) wins. Without a binding (or with an
/// agent that names no model) this returns None, `RAG_ANSWER_MODEL_META` is
/// left unstamped, and the shell's answer node falls back to the platform
/// `rag-llm` alias — so such a project still gets an answer, on the platform
/// model, instead of the llm node's hard "no model" error.
fn project_chat_model(project_id: &str) -> Option<String> {
    let pool = crate::project_studio::project_db::open(project_id).ok()?;
    let raw = crate::project_studio::repository::get_setting(&pool, "agents").ok()??;
    let map: std::collections::HashMap<String, String> = serde_json::from_str(&raw).ok()?;
    let agent_id = map.get("chat").filter(|s| !s.is_empty())?;
    let (_name, model) = crate::project_studio::repository::resolve_agent_label(agent_id)?;
    if model.is_empty() {
        None
    } else {
        Some(model)
    }
}

fn project_studio_chat_stream_handler(
    req: MessageBody,
    ctx: HandlerContext,
    sub: Arc<Subscription>,
) {
    use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin, FlowRequestMeta};
    use crate::flow_engine::envelope::{EnvelopeDelta, FlowEnvelope, FlowValue};
    use tentaflow_protocol::project_studio::ProjectStudioPayload;

    let (project_id, chat_id, message) = match req {
        MessageBody::ProjectStudioBody(ProjectStudioPayload::ChatStreamRequest {
            project_id,
            chat_id,
            message,
        }) => (project_id, chat_id, message),
        _ => {
            let _ = push_end(&sub, None);
            return;
        }
    };

    let end_error = |sub: &Arc<Subscription>, chat_id: &str, error: String| {
        let _ = push_end(
            sub,
            Some(MessageBody::ProjectStudioBody(
                ProjectStudioPayload::ChatStreamEnd {
                    chat_id: chat_id.to_string(),
                    status: "error".into(),
                    error: Some(error),
                    message_id: String::new(),
                },
            )),
        );
    };

    // Guards, uniform denial (no existence leak): project_studio.read →
    // project in the caller's org → REAL membership (chats are personal
    // content, so no project_studio.admin bypass) → the chat belongs to the
    // caller (get_chat filters by user_id).
    let Some(org) = ctx.org_context.as_ref() else {
        end_error(&sub, &chat_id, "chat not found".into());
        return;
    };
    if !org.has("project_studio.read") {
        end_error(&sub, &chat_id, "chat not found".into());
        return;
    }
    let project = match crate::project_studio::repository::get_project(&org.org_id, &project_id) {
        Ok(Some(p)) => p,
        _ => {
            end_error(&sub, &chat_id, "chat not found".into());
            return;
        }
    };
    // Archived projects are read-only — same contract as `require_active` in
    // the request/response handlers (bad_request "project is archived"), so
    // the UI shows the actual state instead of a phantom "chat not found".
    if project.status == "archived" {
        end_error(&sub, &chat_id, "project is archived".into());
        return;
    }
    if !matches!(
        crate::project_studio::repository::member_role(&project_id, &org.user_id),
        Ok(Some(_))
    ) {
        end_error(&sub, &chat_id, "chat not found".into());
        return;
    }
    let chat =
        match crate::project_studio::repository::get_chat(&project_id, &chat_id, &org.user_id) {
            Ok(Some(c)) => c,
            _ => {
                end_error(&sub, &chat_id, "chat not found".into());
                return;
            }
        };

    let message = message.trim().to_string();
    if message.is_empty() {
        end_error(&sub, &chat_id, "message is required".into());
        return;
    }

    let user_role = match &ctx.session {
        SessionAuth::UserSession { role, .. } => role.clone(),
        _ => None,
    };
    let caller_id = org.user_id.clone();
    let org_id = org.org_id.clone();
    let model = project_chat_model(&project_id);
    let router = ctx.state.router.clone();
    let db = ctx.state.db.clone();

    tokio::spawn(async move {
        let Some(fd) = router.flow_dispatcher().cloned() else {
            end_error(&sub, &chat_id, "flow dispatcher unavailable".into());
            return;
        };

        // Persist the user message BEFORE dispatch: conversation_history
        // replays it as the last user turn (the llm builder deduplicates the
        // Text payload against it), and the question survives even when the
        // generation fails mid-stream.
        let db_persist = db.clone();
        let session_for_persist = chat.session_id.clone();
        let user_text = message.clone();
        let persisted = tokio::task::spawn_blocking(move || {
            crate::db::repository::append_project_chat_message(
                &db_persist,
                &session_for_persist,
                "user",
                &user_text,
                None,
            )
        })
        .await;
        if !matches!(persisted, Ok(Ok(_))) {
            end_error(&sub, &chat_id, "failed to persist message".into());
            return;
        }
        let _ = crate::project_studio::repository::touch_chat(&project_id, &chat_id, &caller_id);

        let mut envelope = FlowEnvelope::empty();
        envelope.payload = FlowValue::Text(message);
        envelope.meta.insert(
            "project_id".into(),
            serde_json::Value::String(project_id.clone()),
        );
        envelope.meta.insert(
            "session_id".into(),
            serde_json::Value::String(chat.session_id.clone()),
        );
        // Projekt nie ma kolekcji grafowej, a brak klucza znaczy "graf wlaczony"
        // (zgodnosc wstecz). Wezly grafowe i tak zdegradowalyby sie do
        // pass-through, ale jawne `false` oszczedza im calej pracy.
        envelope
            .meta
            .insert("graph_enabled".into(), serde_json::Value::Bool(false));
        // The shell reads its answer model from a DEDICATED meta entry, not from
        // `model`: routing seeds `model` with the flow's own published name when
        // the addon asks it by name, and the answer node must never dispatch the
        // shell to itself. No project model = the shell's `rag-llm` fallback.
        if let Some(m) = model {
            envelope.meta.insert(
                crate::db::seed::RAG_ANSWER_MODEL_META.into(),
                serde_json::Value::String(m),
            );
        }
        // Terminal shape: the chat streams, so the shell's output node must not
        // rewrite the answer into the addon's `{answer, citations}` JSON. The
        // stamp belongs to the entry point (never to the model's content), same
        // rule as the vector scope minted below.
        envelope.meta.insert(
            crate::flow_engine::node_adapters::output::OUTPUT_MODE_META.into(),
            serde_json::Value::String(
                crate::flow_engine::node_adapters::output::OUTPUT_MODE_STREAM.into(),
            ),
        );

        // Cancel bound to the subscription: a dropped/unsubscribed client
        // aborts the flow (push failure below fires this token).
        let cancel = CancellationToken::new();
        // §2.5 — project chat. Membership was verified before this point, so
        // `caller_id` is an authorized user, not a claim from the request.
        // §3 invariant 1 — minted after the membership check, unique per run.
        // `ctx.correlation_id` is the client's per-connection frame id and
        // collides across connections, so it cannot be the audit join key.
        let correlation_key = uuid::Uuid::new_v4().to_string();
        envelope.meta.insert(
            "correlation_id".into(),
            serde_json::Value::String(correlation_key.clone()),
        );
        let mut meta = FlowRequestMeta::new(
            format!("ps-chat-{correlation_key}"),
            FlowOrigin::Project,
            FlowActor::user(caller_id.clone()),
        );
        meta.correlation_id = Some(correlation_key);
        meta.session_id = Some(chat.session_id.clone());
        meta.user_id = Some(caller_id.clone());
        meta.user_role = user_role;
        meta.org_id = Some(org_id);
        // Scope przestrzeni wektorowej projektu. Mintowany TUTAJ, czyli PO
        // sprawdzeniu czlonkostwa i po `get_chat` filtrowanym po user_id — nigdy z
        // wiadomosci modelu. Wzorzec `ps_generation`: to, na czym flow pracuje, ma
        // byc wiazaniem serwera, a nie parametrem, ktory da sie przekierowac.
        meta.addon_id = Some(crate::project_studio::ingest::vector_scope(&project_id));
        meta.cancel_token = cancel.clone();

        let dispatch = fd
            .dispatch_by_flow_id_streaming(
                crate::db::seed::RAG_QUERY_FLOW_ID.to_string(),
                envelope,
                meta,
            )
            .await;
        let exec = match dispatch {
            Ok(e) => e,
            Err(e) => {
                end_error(&sub, &chat_id, format!("dispatch failed: {e}"));
                return;
            }
        };

        let mut stream = exec.stream;
        let mut full_text = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(EnvelopeDelta::Llm(c)) => {
                    if c.text_delta.is_empty() {
                        continue;
                    }
                    full_text.push_str(&c.text_delta);
                    let chunk = ProjectStudioPayload::ChatStreamChunk {
                        chat_id: chat_id.clone(),
                        kind: "token".into(),
                        text: c.text_delta,
                        citations_json: String::new(),
                    };
                    if push_chunk_async(&sub, MessageBody::ProjectStudioBody(chunk))
                        .await
                        .is_err()
                    {
                        cancel.cancel();
                        return;
                    }
                }
                // The project chat turn is text-only; other delta kinds
                // carry no tokens.
                Ok(_) => {}
                Err(e) => {
                    cancel.cancel();
                    end_error(&sub, &chat_id, format!("stream error: {e}"));
                    return;
                }
            }
        }

        // The final envelope carries the retrieval citations accumulated by the
        // shell's retrieval loop and pinned by `rag_finalize`
        // (meta["rag_citations"]).
        let (citations_json, flow_error) = match exec.outcome.await {
            Ok(outcome) => {
                let cites = outcome
                    .final_envelope
                    .meta
                    .get("rag_citations")
                    .and_then(|v| v.as_array().filter(|a| !a.is_empty()).map(|_| v.clone()))
                    .map(|v| v.to_string());
                (cites, outcome.error)
            }
            // The outcome channel dropped — the flow died without reporting.
            // Don't persist the partial full_text as a successful reply.
            Err(_) => {
                end_error(&sub, &chat_id, "flow execution failed".into());
                return;
            }
        };
        if let Some(e) = flow_error {
            end_error(&sub, &chat_id, e);
            return;
        }

        // Persist the assistant reply (content + citations, so
        // ChatHistoryResponse rebuilds the sources panel) BEFORE pushing the
        // citations chunk: a failed push must not lose the reply — the
        // message_id travels in ChatStreamEnd, and even if that push fails
        // too the reply survives in the history.
        let db_persist = db.clone();
        let session_for_persist = chat.session_id.clone();
        let assistant_text = full_text;
        let cites_for_persist = citations_json.clone();
        let message_id = tokio::task::spawn_blocking(move || {
            crate::db::repository::append_project_chat_message(
                &db_persist,
                &session_for_persist,
                "assistant",
                &assistant_text,
                cites_for_persist.as_deref(),
            )
        })
        .await;
        let message_id = match message_id {
            Ok(Ok(id)) => id.to_string(),
            _ => {
                end_error(&sub, &chat_id, "failed to persist reply".into());
                return;
            }
        };
        let _ = crate::project_studio::repository::touch_chat(&project_id, &chat_id, &caller_id);

        if let Some(cites) = citations_json.as_deref() {
            let chunk = ProjectStudioPayload::ChatStreamChunk {
                chat_id: chat_id.clone(),
                kind: "citations".into(),
                text: String::new(),
                citations_json: cites.to_string(),
            };
            if push_chunk_async(&sub, MessageBody::ProjectStudioBody(chunk))
                .await
                .is_err()
            {
                return;
            }
        }

        let _ = push_end_async(
            &sub,
            Some(MessageBody::ProjectStudioBody(
                ProjectStudioPayload::ChatStreamEnd {
                    chat_id,
                    status: "success".into(),
                    error: None,
                    message_id,
                },
            )),
        )
        .await;
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "ProjectStudioChatStreamRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: project_studio_chat_stream_handler,
    }
}

// =============================================================================
// ProjectStudioRunAutoStream — live view of an automated run (F3, T10).
// =============================================================================
// `auto_runs` emits one log_bus line per poll delta under key = run_id; this
// handler turns those markers into wire chunks by re-reading the row the marker
// names. Polling RunAutoGetRequest stays the source of truth — a subscriber
// that joins late simply starts from the next delta.

/// Shared IDOR guard of the Project Studio stream handlers: read permission,
/// project inside the caller's org and real membership (or
/// `project_studio.admin` for the inspection tier). Returns the project record.
fn project_studio_stream_guard(
    ctx: &HandlerContext,
    project_id: &str,
) -> Option<crate::project_studio::models::ProjectRecord> {
    let org = ctx.org_context.as_ref()?;
    if !org.has("project_studio.read") {
        return None;
    }
    let project = crate::project_studio::repository::get_project(&org.org_id, project_id).ok()??;
    let is_member = matches!(
        crate::project_studio::repository::member_role(project_id, &org.user_id),
        Ok(Some(_))
    );
    if !is_member && !org.has("project_studio.admin") {
        return None;
    }
    Some(project)
}

/// Builds the wire item for one automated run item (used by the "item" marker).
fn auto_item_chunk(
    pool: &crate::db::DbPool,
    run_id: &str,
    item_id: &str,
) -> Option<tentaflow_protocol::project_studio::TestRunItemAutoWire> {
    use tentaflow_protocol::project_studio::ArtifactRef;
    let item = crate::project_studio::auto_runs::list_auto_items(pool, run_id)
        .ok()?
        .into_iter()
        .find(|i| i.item_id == item_id)?;
    let artifact_refs: Vec<ArtifactRef> =
        crate::project_studio::auto_runs::list_artifacts(pool, run_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.item_id == item_id)
            .map(|a| ArtifactRef {
                artifact_id: a.artifact_id.clone(),
                name: a.name,
                kind: a.kind,
                size_bytes: a.size_bytes,
                mime: a.mime,
                download_ref: a.artifact_id,
            })
            .collect();
    Some(tentaflow_protocol::project_studio::TestRunItemAutoWire {
        artifact_refs,
        item_id: item.item_id,
        case_id: item.case_id,
        case_title: item.case_title,
        kind: item.kind,
        language: item.language,
        position: item.position,
        status: item.status,
        duration_ms: item.duration_ms,
        message: item.message,
        steps_total: item.steps_total,
        steps_done: item.steps_done,
    })
}

fn project_studio_run_auto_stream_handler(
    req: MessageBody,
    ctx: HandlerContext,
    sub: Arc<Subscription>,
) {
    use tentaflow_protocol::project_studio::{ArtifactRef, ProjectStudioPayload};

    let (project_id, run_id) = match req {
        MessageBody::ProjectStudioBody(ProjectStudioPayload::RunAutoStreamRequest {
            project_id,
            run_id,
        }) => (project_id, run_id),
        _ => {
            let _ = push_end(&sub, None);
            return;
        }
    };
    if project_studio_stream_guard(&ctx, &project_id).is_none() {
        let _ = push_end(&sub, None);
        return;
    }
    // The run MUST belong to this project: run_id is a process-global log_bus
    // key, so without this check any member of any project could watch it.
    let Ok(pool) = crate::project_studio::project_db::open(&project_id) else {
        let _ = push_end(&sub, None);
        return;
    };
    if !matches!(
        crate::project_studio::runs::get_run(&pool, &run_id),
        Ok(Some(_))
    ) {
        let _ = push_end(&sub, None);
        return;
    }

    tokio::spawn(async move {
        let mut rx = match crate::deploy::log_bus::subscribe(&run_id) {
            Some(rx) => rx,
            None => {
                // No live watcher — the run already settled. The frontend
                // reconciles via RunAutoGetRequest.
                let _ = push_end(&sub, None);
                return;
            }
        };
        use crate::deploy::log_bus::BusMessage;
        loop {
            match rx.recv().await {
                Ok(BusMessage::Line(line)) => {
                    let mut chunk = ProjectStudioPayload::RunAutoStreamChunk {
                        run_id: run_id.clone(),
                        kind: line.kind.clone(),
                        line: String::new(),
                        phase: line.phase.clone(),
                        item: None,
                        perf_json: String::new(),
                        artifact: None,
                        ts_ms: line.ts_ms,
                    };
                    // The marker payload rides in `line`; resolve it to the row
                    // it names so the client gets a full snapshot, not an id.
                    if let ProjectStudioPayload::RunAutoStreamChunk {
                        line: out_line,
                        item,
                        artifact,
                        perf_json,
                        ..
                    } = &mut chunk
                    {
                        match line.kind.as_str() {
                            "item" => *item = auto_item_chunk(&pool, &run_id, &line.line),
                            "artifact" => {
                                *artifact = crate::project_studio::auto_runs::get_artifact(
                                    &pool, &line.line,
                                )
                                .ok()
                                .flatten()
                                .map(|a| ArtifactRef {
                                    artifact_id: a.artifact_id.clone(),
                                    name: a.name,
                                    kind: a.kind,
                                    size_bytes: a.size_bytes,
                                    mime: a.mime,
                                    download_ref: a.artifact_id,
                                })
                            }
                            "perf" => {
                                if let Ok(Some(meta)) =
                                    crate::project_studio::auto_runs::get_meta(&pool, &run_id)
                                {
                                    *perf_json = serde_json::json!({
                                        "summary": serde_json::from_str::<serde_json::Value>(
                                            &meta.perf_summary_json
                                        )
                                        .unwrap_or_else(|_| serde_json::json!([])),
                                        "timeline": serde_json::from_str::<serde_json::Value>(
                                            &meta.perf_timeline_json
                                        )
                                        .unwrap_or_else(|_| serde_json::json!([])),
                                    })
                                    .to_string();
                                }
                            }
                            _ => *out_line = line.line.clone(),
                        }
                    }
                    if push_chunk(&sub, MessageBody::ProjectStudioBody(chunk)).is_err() {
                        return;
                    }
                }
                Ok(BusMessage::End {
                    final_status,
                    error_message,
                    ..
                }) => {
                    let end = ProjectStudioPayload::RunAutoStreamEnd {
                        run_id: run_id.clone(),
                        status: final_status,
                        error: (!error_message.is_empty()).then_some(error_message),
                    };
                    let _ = push_end(&sub, Some(MessageBody::ProjectStudioBody(end)));
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    let _ = push_end(&sub, None);
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "ProjectStudioRunAutoStreamRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: project_studio_run_auto_stream_handler,
    }
}

// =============================================================================
// ProjectStudioTryRunStream — ephemeral single-case execution (F3, T03).
// =============================================================================
// Nothing is persisted: no run row, no items, no artifacts. The execution lives
// in the try-run registry (client-minted `try_id`, owner-scoped cancel, 5 min
// TTL) and its output goes straight to this subscription.

fn project_studio_try_run_stream_handler(
    req: MessageBody,
    ctx: HandlerContext,
    sub: Arc<Subscription>,
) {
    use crate::project_studio::{auto_runs, environments, generation};
    use tentaflow_protocol::project_studio::ProjectStudioPayload;

    let (project_id, try_id, case_id, environment_id, content_override, language, perf_profile_json) =
        match req {
            MessageBody::ProjectStudioBody(ProjectStudioPayload::TryRunStartRequest {
                project_id,
                try_id,
                case_id,
                environment_id,
                content_json_override,
                language,
                perf_profile_json,
            }) => (
                project_id,
                try_id,
                case_id,
                environment_id,
                content_json_override,
                language,
                perf_profile_json,
            ),
            _ => {
                let _ = push_end(&sub, None);
                return;
            }
        };

    let end_error = |sub: &Arc<Subscription>, try_id: &str, error: String| {
        let _ = push_end(
            sub,
            Some(MessageBody::ProjectStudioBody(
                ProjectStudioPayload::TryRunStreamEnd {
                    try_id: try_id.to_string(),
                    status: "error".into(),
                    error: Some(error),
                    junit_summary_json: String::new(),
                },
            )),
        );
    };

    // `try_id` is client-minted and becomes a registry key — bound the charset
    // like `upload_id` so it can never be abused as a path or a wildcard.
    if try_id.is_empty()
        || try_id.len() > 128
        || !try_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        end_error(&sub, &try_id, "invalid try_id".into());
        return;
    }
    let Some(org) = ctx.org_context.as_ref() else {
        end_error(&sub, &try_id, "case not found".into());
        return;
    };
    let Some(project) = project_studio_stream_guard(&ctx, &project_id) else {
        end_error(&sub, &try_id, "case not found".into());
        return;
    };
    if project.status == "archived" {
        end_error(&sub, &try_id, "project is archived".into());
        return;
    }
    // A try run executes untrusted code — the editor tier, not the read tier.
    if !matches!(
        crate::project_studio::repository::effective_role(&project_id, &org.user_id),
        Ok(Some(role)) if role >= crate::project_studio::models::ProjectRole::Editor
    ) {
        end_error(&sub, &try_id, "requires the editor project role".into());
        return;
    }

    let user_id = org.user_id.clone();
    let core_db = ctx.state.db.clone();
    let cipher = ctx.state.settings_cipher.clone();
    let node_id = ctx.state.local_node_id.clone();

    tokio::spawn(async move {
        let Ok(pool) = crate::project_studio::project_db::open(&project_id) else {
            end_error(&sub, &try_id, "project storage unavailable".into());
            return;
        };
        let environment = match environments::get(&pool, &environment_id) {
            Ok(Some(env)) => env,
            _ => {
                end_error(&sub, &try_id, "unknown environment".into());
                return;
            }
        };
        if environment.approval_status != "approved" {
            end_error(
                &sub,
                &try_id,
                format!(
                    "environment '{}' is not approved (status '{}')",
                    environment.name, environment.approval_status
                ),
            );
            return;
        }
        // The approval was taken on a DNS answer that may have moved since: a
        // host approved as public can point at loopback or the metadata address
        // by now, so the class is re-derived right before the runner connects.
        let probe = environment.clone();
        let now_private = tokio::task::spawn_blocking(move || environments::recheck_private(&probe))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(true);
        if now_private && !environment.is_private_address {
            crate::project_studio::activity::record_org_security(
                &core_db,
                &node_id,
                &user_id,
                "project_studio.environment_address_rebinding_denied",
                &format!(
                    "project:{project_id}/environment:{}",
                    environment.environment_id
                ),
                &serde_json::json!({ "base_url": environment.base_url }).to_string(),
            );
            end_error(
                &sub,
                &try_id,
                format!(
                    "environment '{}' now resolves to a private address — it was approved as \
                     a public target, so the try run was refused",
                    environment.name
                ),
            );
            return;
        }
        let case = match crate::project_studio::tests::get_case(&pool, &case_id) {
            Ok(Some(case)) => case,
            _ => {
                end_error(&sub, &try_id, "case not found".into());
                return;
            }
        };
        if !generation::is_code_kind(&case.record.kind) {
            end_error(&sub, &try_id, "only code cases can be try-run".into());
            return;
        }
        let language = if language.trim().is_empty() {
            case.record.language.clone()
        } else {
            language.trim().to_ascii_lowercase()
        };
        let content_json = if content_override.trim().is_empty() {
            case.record.content_json.clone()
        } else {
            content_override
        };
        let mut content: serde_json::Value =
            serde_json::from_str(&content_json).unwrap_or_else(|_| serde_json::json!({}));
        if case.record.kind == "perf" && !perf_profile_json.trim().is_empty() {
            if let Ok(profile) = serde_json::from_str::<serde_json::Value>(&perf_profile_json) {
                if let Some(map) = content.as_object_mut() {
                    map.insert("profile".to_string(), profile);
                }
            }
        }
        // Validated AFTER the merge: the override is what actually runs, so
        // validating the stored case first would leave the requested
        // users/spawn_rate/duration unbounded.
        if let Err(message) =
            generation::validate_case_content(&case.record.kind, &language, &content.to_string())
        {
            end_error(&sub, &try_id, message);
            return;
        }
        // A unit case bound to a build profile cannot be executed: the runner
        // needs an inline profile with an absolute mounted workdir, which
        // requires the per-run sandbox. Submitting it would degrade to plain
        // pytest in an empty directory and report a green run.
        if case.record.kind == "unit"
            && content
                .get("build_profile_ref")
                .and_then(|v| v.as_str())
                .is_some_and(|v| !v.trim().is_empty())
        {
            end_error(
                &sub,
                &try_id,
                "running a build profile requires a per-run sandbox (planned) — the case \
                 was not executed"
                    .into(),
            );
            return;
        }

        let discovery_db = core_db.clone();
        let runners = match tokio::task::spawn_blocking(move || {
            auto_runs::list_runners(&discovery_db)
        })
        .await
        {
            Ok(Ok(runners)) => runners,
            _ => {
                end_error(&sub, &try_id, "runner discovery failed".into());
                return;
            }
        };
        let runner = match auto_runs::select_runner(runners, "", &language) {
            Ok(runner) => runner,
            Err(e) => {
                end_error(&sub, &try_id, e.to_string());
                return;
            }
        };
        if let Some(reason) = auto_runs::isolation_refusal(&core_db, &runner) {
            end_error(&sub, &try_id, reason);
            return;
        }

        let Some(cancel) = auto_runs::register_try_run(&try_id, &user_id) else {
            end_error(&sub, &try_id, "this try_id is already running".into());
            return;
        };
        // No auth type = nothing to authenticate with; a stored leftover secret
        // must not reach the runner (and from there the test script).
        let secret = if environment.auth_type == "none" {
            String::new()
        } else {
            match environments::decrypt_secret(&cipher, &environment) {
                Ok(secret) => secret,
                Err(_) => {
                    auto_runs::unregister_try_run(&try_id);
                    end_error(&sub, &try_id, "environment secret unavailable".into());
                    return;
                }
            }
        };
        let submit_env = auto_runs::SubmitEnvironment {
            base_url: environment.base_url.clone(),
            auth_type: environment.auth_type.clone(),
            secret,
            extra_headers: serde_json::from_str(&environment.extra_headers_json)
                .unwrap_or_else(|_| serde_json::json!({})),
            host_allowlist: environments::host_allowlist_of(&environment),
        };
        let item = auto_runs::SubmitItem {
            item_id: format!("try-{}", &try_id[..try_id.len().min(64)]),
            kind: case.record.kind.clone(),
            language: language.clone(),
            config: content
                .get("config")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
            content,
        };

        // A try run occupies a runner slot and executes untrusted code; without
        // a gate one user's editor could keep every slot busy. The permit is
        // held for the whole execution and released when this task ends.
        let _permit = match auto_runs::try_run_semaphore().acquire().await {
            Ok(permit) => permit,
            Err(_) => {
                auto_runs::unregister_try_run(&try_id);
                end_error(&sub, &try_id, "try run queue unavailable".into());
                return;
            }
        };

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
        let endpoint = runner.endpoint_url.clone();
        let try_for_task = try_id.clone();
        let cancel_for_task = cancel.clone();
        let deadline_ms = crate::deploy::log_bus::now_ms()
            + auto_runs::TRY_RUN_TTL.as_millis() as i64;
        let execution = tokio::task::spawn_blocking(move || {
            auto_runs::run_try_item(
                &endpoint,
                &try_for_task,
                &item,
                &submit_env,
                &cancel_for_task,
                deadline_ms,
                |kind, line| {
                    let _ = event_tx.send((kind.to_string(), line.to_string()));
                },
            )
        });

        while let Some((kind, line)) = event_rx.recv().await {
            let (kind, phase, line) = if kind == "phase" {
                ("phase".to_string(), line, String::new())
            } else {
                (kind, String::new(), line)
            };
            let chunk = ProjectStudioPayload::TryRunStreamChunk {
                try_id: try_id.clone(),
                kind,
                line,
                phase,
                ts_ms: crate::deploy::log_bus::now_ms(),
            };
            if push_chunk_async(&sub, MessageBody::ProjectStudioBody(chunk))
                .await
                .is_err()
            {
                // The client is gone — stop the execution instead of paying for
                // a run nobody will read.
                auto_runs::cancel_try_run(&try_id, &user_id);
                break;
            }
        }

        let outcome = execution.await;
        auto_runs::unregister_try_run(&try_id);
        let (status, error, summary) = match outcome {
            Ok(Ok(summary)) => {
                let status = summary
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error")
                    .to_string();
                (status, None, summary.to_string())
            }
            Ok(Err(e)) => ("error".to_string(), Some(e.to_string()), String::new()),
            Err(_) => (
                "error".to_string(),
                Some("try run task panicked".to_string()),
                String::new(),
            ),
        };
        let _ = push_end_async(
            &sub,
            Some(MessageBody::ProjectStudioBody(
                ProjectStudioPayload::TryRunStreamEnd {
                    try_id,
                    status,
                    error,
                    junit_summary_json: summary,
                },
            )),
        )
        .await;
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "ProjectStudioTryRunStartRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: project_studio_try_run_stream_handler,
    }
}

// =============================================================================
// ProjectStudioCodeAssistStream — AI assist for the code editor (F3, T03).
// =============================================================================
// Routed through the project's `generator_<kind>` agent (its model + system
// prompt) over the platform flow gateway, so the turn lands in the compliance
// AI-event trail exactly like the batch generator — never a raw chat
// completion.
//
// The dispatch resolves the USER-DEFINED flow for `{model}:chat`, so whatever
// blocks that flow carries (tools, memory, RAG) run for this turn too — this is
// not a tool-free path. Only the final text is used (the proposal), so a tool
// call cannot change what the editor inserts, but a flow with side-effecting
// tools would execute them. A dedicated system flow (the `core:rag-query`
// pattern) would close that off; it needs its own seed + streaming contract,
// so it is deliberately NOT bolted on here.

fn project_studio_code_assist_stream_handler(
    req: MessageBody,
    ctx: HandlerContext,
    sub: Arc<Subscription>,
) {
    use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin, FlowRequestMeta};
    use crate::flow_engine::envelope::{EnvelopeDelta, FlowEnvelope, FlowValue};
    use crate::project_studio::code_assist;
    use tentaflow_protocol::project_studio::ProjectStudioPayload;

    let (project_id, case_id, kind, selection, instruction, full_content) = match req {
        MessageBody::ProjectStudioBody(ProjectStudioPayload::CodeAssistRequest {
            project_id,
            case_id,
            kind,
            selection,
            instruction,
            full_content,
        }) => (
            project_id,
            case_id,
            kind,
            selection,
            instruction,
            full_content,
        ),
        _ => {
            let _ = push_end(&sub, None);
            return;
        }
    };

    let end_error = |sub: &Arc<Subscription>, error: String| {
        let _ = push_end(
            sub,
            Some(MessageBody::ProjectStudioBody(
                ProjectStudioPayload::CodeAssistStreamEnd {
                    proposal: String::new(),
                    error: Some(error),
                },
            )),
        );
    };

    let Some(org) = ctx.org_context.as_ref() else {
        end_error(&sub, "case not found".into());
        return;
    };
    let Some(project) = project_studio_stream_guard(&ctx, &project_id) else {
        end_error(&sub, "case not found".into());
        return;
    };
    if project.status == "archived" {
        end_error(&sub, "project is archived".into());
        return;
    }
    if !matches!(
        crate::project_studio::repository::effective_role(&project_id, &org.user_id),
        Ok(Some(role)) if role >= crate::project_studio::models::ProjectRole::Editor
    ) {
        end_error(&sub, "requires the editor project role".into());
        return;
    }

    let Ok(pool) = crate::project_studio::project_db::open(&project_id) else {
        end_error(&sub, "project storage unavailable".into());
        return;
    };
    // The kind of the SAVED case wins over the wire value when the case exists,
    // so a client cannot pick a different agent for an existing case.
    let (kind, language) = match crate::project_studio::tests::get_case(&pool, &case_id) {
        Ok(Some(case)) => (case.record.kind, case.record.language),
        _ => (kind, "python".to_string()),
    };
    let request = code_assist::AssistRequest {
        kind: &kind,
        language: &language,
        selection: &selection,
        instruction: &instruction,
        full_content: &full_content,
    };
    if let Err(e) = code_assist::validate(&request) {
        end_error(&sub, e.to_string());
        return;
    }
    let agent = match code_assist::resolve_agent(&ctx.state.db, &pool, &kind) {
        Ok(agent) => agent,
        Err(e) => {
            end_error(&sub, e.to_string());
            return;
        }
    };
    let system = code_assist::system_prompt(&agent, &kind, &language);
    let user_turn = code_assist::user_prompt(&request);

    let user_role = match &ctx.session {
        SessionAuth::UserSession { role, .. } => role.clone(),
        _ => None,
    };
    let caller_id = org.user_id.clone();
    let org_id = org.org_id.clone();
    let router = ctx.state.router.clone();

    tokio::spawn(async move {
        let Some(fd) = router.flow_dispatcher().cloned() else {
            end_error(&sub, "flow dispatcher unavailable".into());
            return;
        };
        let mut envelope = FlowEnvelope::empty();
        envelope.payload = FlowValue::Text(user_turn);
        envelope.context.system_prompts.push(system);
        envelope.meta.insert(
            "project_id".into(),
            serde_json::Value::String(project_id.clone()),
        );
        // The audit event carries the agent this assist acted as.
        envelope
            .meta
            .insert("agent_id".into(), serde_json::Value::String(agent.agent_id));

        let cancel = CancellationToken::new();
        // §2.5 — Code Studio assist.
        // §3 invariant 1 — minted after authorization, unique per run (see the
        // dashboard chat entry point for why the frame id cannot serve here).
        let correlation_key = uuid::Uuid::new_v4().to_string();
        envelope.meta.insert(
            "correlation_id".into(),
            serde_json::Value::String(correlation_key.clone()),
        );
        let mut meta = FlowRequestMeta::new(
            format!("ps-assist-{correlation_key}"),
            FlowOrigin::CodeStudio,
            FlowActor::user(caller_id.clone()),
        );
        meta.correlation_id = Some(correlation_key);
        meta.user_id = Some(caller_id);
        meta.user_role = user_role;
        meta.org_id = Some(org_id);
        meta.cancel_token = cancel.clone();

        let exec = match fd
            .try_dispatch_streaming(&agent.model, "chat", envelope, meta)
            .await
        {
            Ok(exec) => exec,
            Err(e) => {
                end_error(&sub, format!("dispatch failed: {e}"));
                return;
            }
        };

        let mut stream = exec.stream;
        let mut full_text = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(EnvelopeDelta::Llm(c)) => {
                    if c.text_delta.is_empty() {
                        continue;
                    }
                    full_text.push_str(&c.text_delta);
                    let chunk = ProjectStudioPayload::CodeAssistStreamChunk {
                        token: c.text_delta,
                    };
                    if push_chunk_async(&sub, MessageBody::ProjectStudioBody(chunk))
                        .await
                        .is_err()
                    {
                        cancel.cancel();
                        return;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    cancel.cancel();
                    end_error(&sub, format!("stream error: {e}"));
                    return;
                }
            }
        }
        if let Ok(outcome) = exec.outcome.await {
            if let Some(e) = outcome.error {
                end_error(&sub, e);
                return;
            }
        }
        let _ = push_end_async(
            &sub,
            Some(MessageBody::ProjectStudioBody(
                ProjectStudioPayload::CodeAssistStreamEnd {
                    proposal: code_assist::clean_proposal(&full_text),
                    error: None,
                },
            )),
        )
        .await;
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "ProjectStudioCodeAssistRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: project_studio_code_assist_stream_handler,
    }
}

// =============================================================================
// Code Studio streams (§12.2) — session timeline, terminal grid, index.
// =============================================================================
// Three subscriptions with one contract: the CLIENT holds the cursor and the
// server resumes from it — `after_seq` walks the event log, `after_revision`
// walks the VT grid. Neither stream keeps a ring buffer of the last N frames,
// and that is deliberate: the event log IS the durable buffer (§13.3 — events
// are the source of truth, the status columns are a projection), and the VT
// grid tags every row with the revision at which it last changed. A ring buffer
// next to either would be a second, weaker copy that can fall behind and then
// has to be reconciled against the real one anyway.
//
// Backpressure is the credit window of the subscription channel: every frame
// leaves through `push_chunk_async`, which AWAITS a free slot, so the producer
// — the loop reading the log or the grid — blocks with a slow browser instead
// of growing an unbounded queue. What does not fit into a frame is not buffered
// either: an event body over the budget travels as the artifact reference it
// already has (§12.2, §13.2), never as a truncated copy that would look whole.
//
// The terminal never carries raw VT bytes (§7.9). The parser runs here, on the
// owner node, so a container and a remote node render identically and the
// browser needs no terminal emulator.
//
// A workspace whose owner node is not this one is served the same way from the
// browser's point of view, and completely differently underneath: the timeline
// and the terminal are PULLED from the owner node's stream hub
// (`code_studio::mesh_stream`) and pushed into the same subscription, frame for
// frame, with the same variants the local loops emit. The pull carries the
// acknowledgement that returns credit to the producer, so backpressure reaches
// all the way from the browser to the process writing the output on the other
// node. The gate still runs HERE, every `CS_REVALIDATE_EVERY`, on this node's
// registry: a proxy that stopped checking would let a revoked membership keep
// reading for as long as the socket lived.

use crate::code_studio::mesh_stream;
use crate::code_studio::terminal::{
    Cell, Color, Cursor, GridRow, PtyHandle, TerminalRegistry, TerminalState,
};
use crate::mesh::iroh_manager::IrohMeshManager;
use tentaflow_protocol::code_studio::{CodeStudioPayload, TerminalCellRow, TerminalCursorInfo};

/// The session was closed or interrupted; its timeline is final. It is the hub
/// reason too — a stream close travelling over the mesh and the end of a local
/// subscription are the same event and must not carry two different words.
const CS_END_SESSION_CLOSED: &str = mesh_stream::REASON_SESSION_CLOSED;
/// Uniform denial: a non-member must not be able to tell a missing workspace
/// from someone else's, and a session from someone else's session.
const CS_END_NOT_FOUND: &str = "not_found";
/// Membership, `code_studio.read` or session ownership went away mid-stream.
/// Also travels over the mesh as a stream close reason — one definition, so the
/// owner node and the browser cannot name the same event differently.
pub(crate) const CS_END_PERMISSION_REVOKED: &str = mesh_stream::REASON_PERMISSION_REVOKED;
/// The workspace belongs to another node and THIS stream has no remote path:
/// the index stream, whose progress channel nobody publishes to the mesh hub,
/// or a workspace that moved to another node while a local stream was open.
/// The session timeline and the terminal do not end here — they pull from the
/// owner node (§12.2).
const CS_END_NOT_LOCAL: &str = "workspace_not_local";
/// The owner node did not serve the stream: the mesh is not running on this
/// node, the peer is not trusted, it did not answer, or it holds no such
/// stream. All four are availability, never a verdict about the session
/// (§3.5), so the UI may retry — reporting `session_closed` here would be a
/// lie about somebody's unfinished work.
const CS_END_OWNER_UNREACHABLE: &str = "owner_unreachable";
/// A frame from the owner node spent the stream's inline budget and travels as
/// an artifact reference (§12.2). The browser has no way to fetch and render a
/// chunk of a timeline out of band, so the stream says so rather than dropping
/// output that the client would never learn it was missing.
const CS_END_STREAM_OVERFLOW: &str = "stream_overflow";
/// No such terminal on this node. A Core restart reaps every shell (§1.2 D2),
/// so a browser resuming after one has to open a new terminal.
const CS_END_TERMINAL_NOT_OPEN: &str = "terminal_not_open";
/// The shell ended; the grid stops at its last output.
const CS_END_TERMINAL_EXITED: &str = "terminal_exited";
/// The semantic index is switched off for this workspace, so no job will ever
/// publish progress. An open stream that can never carry a frame is
/// indistinguishable from a stalled indexer, which is why this is stated.
const CS_END_INDEX_UNAVAILABLE: &str = "index_unavailable";
/// The server could not read its own state.
pub(crate) const CS_END_INTERNAL: &str = "internal_error";

/// Events read per catch-up page. A resume from `after_seq = 0` on a long
/// session pages through the log instead of materialising it at once.
const CS_EVENT_PAGE: usize = 256;

/// How long a REMOTE timeline waits after an empty batch. The local timeline
/// has no interval at all — its writer announces (`events::subscribe`) — but a
/// mesh pull cannot be woken from the other node, so an idle remote session
/// costs one round trip at this cadence and no more.
const CS_SESSION_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// How often the VT grid is compared against the revision already sent. The SLO
/// for a keystroke echo is p95 < 100 ms locally (§22), so the sampling interval
/// has to be an order of magnitude below it; one frame of a 60 Hz display is
/// both small enough and pointless to beat, since nothing renders faster.
const CS_TERMINAL_POLL: std::time::Duration = std::time::Duration::from_millis(16);

/// How often the whole access gate is re-run while a stream is open.
const CS_REVALIDATE_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// Frame budget for one event body. Above it the frame carries the artifact
/// reference instead (§12.2).
const CS_MAX_EVENT_PAYLOAD_BYTES: usize = 16 * 1024;

/// How often an IDLE remote terminal is polled. A mesh pull is a round trip,
/// not a memory read, so sampling it at the local grid's 16 ms would cost some
/// sixty requests a second per open shell that nobody is typing into. §22
/// allows 250 ms p95 for a remote keystroke echo, and a batch that carried
/// frames is followed immediately — this interval only bounds the wait when
/// there was nothing to fetch.
const CS_REMOTE_TERMINAL_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Frames this node is willing to take in one pull from the owner node. It is
/// the credit the producer gets back, so it must not exceed the hub's window.
const CS_PULL_CREDITS: u32 = mesh_stream::DEFAULT_WINDOW;

/// Deadline for one pull round trip. Short: the pull carries no work, it reads
/// a buffer, so a node that needs longer than this is not answering.
const CS_PULL_TIMEOUT_SECS: u64 = 15;

/// Hub id of a session's timeline. The owner node publishes under exactly this
/// id — the pair of ids below is the whole naming contract between the node
/// that produces the frames and the node that shows them.
const CS_STREAM_TIMELINE: &str = "timeline";

/// Hub id of one terminal. The terminal id is part of it because a session can
/// have several shells open at once and each is its own stream.
fn code_studio_terminal_stream_id(terminal_id: &str) -> String {
    format!("terminal:{terminal_id}")
}

/// Hub id of a workspace's indexing progress. The workspace is part of it
/// because this stream has no session to be keyed by, and two workspaces
/// watched from one node must not share a stream.
fn code_studio_index_stream_id(workspace_id: &str) -> String {
    format!("index:{workspace_id}")
}

/// Process-wide VT registries, one per workspace. The grid lives in memory on
/// the owner node, so the stream and the terminal request handlers MUST read the
/// same registry: a second instance would answer with an empty grid while the
/// shell writes into the first.
static CODE_STUDIO_TERMINALS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, Arc<TerminalRegistry>>>,
> = std::sync::OnceLock::new();

/// The workspace's terminal registry, created on first use. Its record root is
/// the workspace `tmp/` directory: a record of a running shell is runtime state
/// of this node and must not be synced.
///
/// This is the ONE holder for the process. Whatever opens a pseudo terminal —
/// `dispatch::code_studio::workspace_runtime` — has to take its registry from
/// here rather than construct its own, or the stream below would poll a grid no
/// shell ever writes to.
pub fn code_studio_terminal_registry(workspace_id: &str) -> anyhow::Result<Arc<TerminalRegistry>> {
    let records_root = crate::code_studio::paths::workspace_dir(workspace_id)?.join("tmp");
    let map = CODE_STUDIO_TERMINALS.get_or_init(|| std::sync::Mutex::new(Default::default()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    Ok(Arc::clone(
        guard
            .entry(workspace_id.to_string())
            .or_insert_with(|| Arc::new(TerminalRegistry::new(records_root))),
    ))
}

/// Access gate of one Code Studio stream, kept so it can be RE-RUN. §12.2
/// closes a stream when the membership, the permission or the session behind it
/// goes away, and that cannot be noticed by checking once at subscribe time.
struct CodeStudioStreamGate {
    db: crate::db::DbPool,
    node_id: String,
    user_id: String,
    org_id: String,
    workspace_id: String,
    /// Empty for the workspace-scoped index stream.
    session_id: String,
}

/// Where a stream's frames are produced.
///
/// A workspace runs on exactly one node (§3), so either this node holds the
/// event log, the VT grid and the runtime database, or another node does and
/// the frames have to be pulled from it over the mesh (§12.2).
enum CodeStudioStreamTarget {
    Local {
        pool: crate::db::DbPool,
        session: crate::code_studio::session::SessionRecord,
    },
    Remote {
        owner_node_id: String,
    },
}

/// Workspace half of the gate: permission, org, membership.
///
/// Says nothing about WHERE the workspace runs — the callers below decide what
/// a foreign owner node means for their stream, because one of them pulls from
/// it and the others cannot.
///
/// Permissions are re-read from the database rather than taken from the
/// connection's snapshot — a stream outlives the request that opened it, so the
/// snapshot would keep a revoked grant alive for as long as the socket lives.
fn code_studio_workspace_gate(
    db: &crate::db::DbPool,
    user_id: &str,
    org_id: &str,
    workspace_id: &str,
) -> Result<crate::code_studio::models::WorkspaceRecord, &'static str> {
    use crate::code_studio::models::WorkspaceStatus;

    crate::code_studio::paths::validate_workspace_id(workspace_id).map_err(|_| CS_END_NOT_FOUND)?;
    let org = crate::services::rbac::resolve_org_context(db, user_id, Some(org_id))
        .map_err(|_| CS_END_PERMISSION_REVOKED)?;
    if !org.has("code_studio.read") {
        return Err(CS_END_PERMISSION_REVOKED);
    }
    let record = crate::code_studio::repository::get_workspace(db, workspace_id)
        .map_err(|_| CS_END_INTERNAL)?
        .ok_or(CS_END_NOT_FOUND)?;
    if record.org_id != org_id || record.status == WorkspaceStatus::Deleted.slug() {
        return Err(CS_END_NOT_FOUND);
    }
    // Membership only. The administrator overlay of §25.4 covers registry
    // metadata and lifecycle, never the content these streams carry.
    if crate::code_studio::repository::role_of(db, workspace_id, user_id)
        .map_err(|_| CS_END_INTERNAL)?
        .is_none()
    {
        return Err(CS_END_NOT_FOUND);
    }
    Ok(record)
}

/// Workspace half of the gate as the MESH path runs it.
///
/// Identical to what a browser attached to this node passes through — the same
/// permission, the same org, the same membership, re-read from the database —
/// plus the rule that this node must actually own the workspace. A workspace
/// that runs elsewhere yields the uniform `not_found`: the caller read a stale
/// registry row, and telling it more would let it map where workspaces live.
pub(crate) fn code_studio_authorize_stream_read(
    db: &crate::db::DbPool,
    local_node_id: &str,
    user_id: &str,
    org_id: &str,
    workspace_id: &str,
) -> Result<crate::code_studio::models::WorkspaceRecord, &'static str> {
    let record = code_studio_workspace_gate(db, user_id, org_id, workspace_id)?;
    if record.node_id != local_node_id {
        return Err(CS_END_NOT_FOUND);
    }
    Ok(record)
}

/// The full gate for a SESSION stream: the workspace half plus the one check
/// only the owner node can make, because only it holds the row — the session
/// belongs to this person and to nobody else (§5.3, §25.4).
pub(crate) fn code_studio_authorize_stream(
    db: &crate::db::DbPool,
    local_node_id: &str,
    user_id: &str,
    org_id: &str,
    workspace_id: &str,
    session_id: &str,
) -> Result<crate::code_studio::session::SessionRecord, &'static str> {
    code_studio_authorize_stream_read(db, local_node_id, user_id, org_id, workspace_id)?;
    crate::code_studio::paths::validate_session_id(session_id).map_err(|_| CS_END_NOT_FOUND)?;
    let pool = crate::code_studio::workspace_db::open(workspace_id).map_err(|_| CS_END_INTERNAL)?;
    let record = crate::code_studio::session::get_session(&pool, session_id)
        .map_err(|_| CS_END_INTERNAL)?
        .ok_or(CS_END_NOT_FOUND)?;
    // Somebody else's session answers exactly like a session that was never
    // opened. An administrator gets the same answer (§25.4).
    if record.user_id != user_id {
        return Err(CS_END_NOT_FOUND);
    }
    Ok(record)
}

impl CodeStudioStreamGate {
    /// Workspace gate for a stream that only this node can serve. Returns the
    /// CURRENT registry row, so a re-run sees a setting that changed under it.
    async fn check_workspace(
        &self,
    ) -> Result<crate::code_studio::models::WorkspaceRecord, &'static str> {
        let (db, node, user, org, workspace) = (
            self.db.clone(),
            self.node_id.clone(),
            self.user_id.clone(),
            self.org_id.clone(),
            self.workspace_id.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let record = code_studio_workspace_gate(&db, &user, &org, &workspace)?;
            if record.node_id != node {
                return Err(CS_END_NOT_LOCAL);
            }
            Ok(record)
        })
        .await
        .unwrap_or(Err(CS_END_INTERNAL))
    }

    /// Workspace gate plus the session, and where the session's frames live.
    ///
    /// For a local workspace it returns the runtime pool and the CURRENT
    /// session row, so a re-run also reports whether the session has since been
    /// closed. For a remote one it names the owner node and stops before
    /// touching a runtime database this node does not have: opening one here
    /// would create an empty workspace directory for somebody else's workspace.
    async fn check_access(&self) -> Result<CodeStudioStreamTarget, &'static str> {
        let (db, node, user, org, workspace, session) = (
            self.db.clone(),
            self.node_id.clone(),
            self.user_id.clone(),
            self.org_id.clone(),
            self.workspace_id.clone(),
            self.session_id.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let workspace_record = code_studio_workspace_gate(&db, &user, &org, &workspace)?;
            if workspace_record.node_id != node {
                // The session row lives in the owner node's runtime database
                // (§5.3), so whether this person owns THAT session is decided
                // by the node that holds it. That node runs the SAME gate
                // (`code_studio_authorize_stream`) against the actor named by
                // the assertion every stream call carries (§12.1), which is
                // what keeps a stream private to one person rather than open to
                // everyone on a trusted node. What the registry knows, this
                // node has just checked: the permission, the org, the
                // membership.
                return Ok(CodeStudioStreamTarget::Remote {
                    owner_node_id: workspace_record.node_id,
                });
            }
            // ONE definition of the session rule, shared with the mesh path.
            let record =
                code_studio_authorize_stream(&db, &node, &user, &org, &workspace, &session)?;
            let pool =
                crate::code_studio::workspace_db::open(&workspace).map_err(|_| CS_END_INTERNAL)?;
            Ok(CodeStudioStreamTarget::Local {
                pool,
                session: record,
            })
        })
        .await
        .unwrap_or(Err(CS_END_INTERNAL))
    }

    /// The node the workspace runs on, once the person has passed the workspace
    /// gate. Used by the workspace-scoped index stream, which has no session to
    /// resolve a target with.
    async fn check_owner_node(&self) -> Result<String, &'static str> {
        let (db, user, org, workspace) = (
            self.db.clone(),
            self.user_id.clone(),
            self.org_id.clone(),
            self.workspace_id.clone(),
        );
        tokio::task::spawn_blocking(move || {
            code_studio_workspace_gate(&db, &user, &org, &workspace).map(|record| record.node_id)
        })
        .await
        .unwrap_or(Err(CS_END_INTERNAL))
    }

    /// Re-check a REMOTE stream from this node's side: the person may still
    /// read this workspace and it still runs where the stream is being pulled
    /// from. The session itself is the owner node's business — it holds the row
    /// — and is re-checked there.
    async fn check_remote_owner(&self, owner_node_id: &str) -> Result<(), &'static str> {
        let (db, user, org, workspace, owner) = (
            self.db.clone(),
            self.user_id.clone(),
            self.org_id.clone(),
            self.workspace_id.clone(),
            owner_node_id.to_string(),
        );
        tokio::task::spawn_blocking(move || {
            let record = code_studio_workspace_gate(&db, &user, &org, &workspace)?;
            if record.node_id != owner {
                // The workspace moved. Whatever it is doing now, it is not this
                // stream on that node.
                return Err(CS_END_NOT_LOCAL);
            }
            Ok(())
        })
        .await
        .unwrap_or(Err(CS_END_INTERNAL))
    }
}

/// Closes a stream with a NAMED reason. Every exit goes through one of these:
/// a silent drop would leave the UI unable to tell "nothing more to show" from
/// "you lost access".
async fn code_studio_end(sub: &Arc<Subscription>, frame: CodeStudioPayload) {
    let _ = push_end_async(sub, Some(MessageBody::CodeStudioBody(frame))).await;
}

// -----------------------------------------------------------------------------
// Owner side: the producers that feed a mesh stream (§12.2)
// -----------------------------------------------------------------------------
//
// There is ONE producer per stream and it is the same source the local
// subscription reads: the event log for a timeline (§3 — the coordinator is the
// only writer of `session_events`), the VT grid for a terminal (§7.9), the
// indexer's progress channel for the index. Nothing here is a second copy of
// the state; a producer encodes exactly the `CodeStudioPayload` frames the
// local path pushes into a subscription, and the consumer node forwards them
// unchanged.
//
// A producer also owns the stream's LIFETIME. It re-runs the whole access gate
// every `CS_REVALIDATE_EVERY` against the database — not against anything
// captured when the stream opened — and closes with a stated reason when the
// membership, the role, the permission or the session ends. That is the half of
// §12.2 a per-call check cannot cover: nobody pulls a stream they have stopped
// being allowed to read, so a stream nobody pulls would otherwise sit there
// producing.

/// How long a closed stream keeps its buffers so the consumer can collect the
/// close record. Dropping them immediately would turn "you lost access" into a
/// stream that simply stopped existing, which is the one thing §12.2 forbids.
const CS_CLOSE_LINGER: std::time::Duration = std::time::Duration::from_secs(30);

/// The streams an owner node produces, parsed from the wire id.
enum CodeStudioOwnerStream {
    Timeline,
    Terminal { terminal_id: String },
    Index,
}

/// Parse and VALIDATE a stream id against the rest of the request.
///
/// The id is not a free-form string: a timeline and a terminal belong to a
/// session, the index belongs to a workspace and names it, and both ids go
/// through the same alphabet guard the filesystem paths use. Anything else is
/// refused before a stream exists.
fn code_studio_owner_stream(
    request: &tentaflow_protocol::mesh::CodeStudioStreamOpenRequest,
) -> Option<CodeStudioOwnerStream> {
    if request.stream_id == CS_STREAM_TIMELINE {
        return (!request.session_id.is_empty()).then_some(CodeStudioOwnerStream::Timeline);
    }
    if let Some(terminal_id) = request.stream_id.strip_prefix("terminal:") {
        if request.session_id.is_empty()
            || crate::code_studio::paths::validate_session_id(terminal_id).is_err()
        {
            return None;
        }
        return Some(CodeStudioOwnerStream::Terminal {
            terminal_id: terminal_id.to_string(),
        });
    }
    if let Some(workspace_id) = request.stream_id.strip_prefix("index:") {
        // The index stream has no session, so its id has to carry the
        // workspace: without it two workspaces watched from one node would
        // collide on a single hub key.
        if !request.session_id.is_empty() || workspace_id != request.workspace_id {
            return None;
        }
        return Some(CodeStudioOwnerStream::Index);
    }
    None
}

/// A terminal's full state, for a consumer whose gap fell out of the replay
/// buffer (§12.2). The grid plus its revision is a complete answer, which is
/// why the terminal is the one stream that resynchronizes instead of closing.
struct CodeStudioTerminalSnapshot {
    registry: Arc<TerminalRegistry>,
    pty: PtyHandle,
}

impl mesh_stream::SnapshotSource for CodeStudioTerminalSnapshot {
    fn snapshot(&self) -> Option<(u64, Vec<u8>)> {
        let grid = self.registry.snapshot(&self.pty).ok()?;
        let payload = CodeStudioPayload::TerminalStreamSnapshot {
            revision: grid.revision,
            grid_rows: grid.rows,
            grid_cols: grid.cols,
            cursor: code_studio_cursor(grid.cursor),
            rows: code_studio_cell_rows(&grid.lines),
        };
        Some((grid.revision, crate::mesh::cbor::encode(&payload).ok()?))
    }
}

/// Open a stream for the actor an assertion named, and start producing.
///
/// Called from the mesh receive path (`remote_proxy::open_owner_stream`) after
/// the assertion verified. Everything authorization-shaped happens HERE, on the
/// node that holds the data, exactly as it does for a browser attached to this
/// node — the assertion transported an identity, it granted nothing.
pub(crate) async fn code_studio_open_owner_stream(
    state: &Arc<crate::dispatch::AppState>,
    verified: &crate::code_studio::assertion::VerifiedAssertion,
    consumer_node_id: &str,
    request: &tentaflow_protocol::mesh::CodeStudioStreamOpenRequest,
) -> Result<u64, &'static str> {
    let kind = code_studio_owner_stream(request).ok_or(CS_END_NOT_FOUND)?;
    let gate = CodeStudioStreamGate {
        db: state.db.clone(),
        node_id: state.local_node_id.to_string(),
        user_id: verified.user_id.clone(),
        org_id: verified.org_id.clone(),
        workspace_id: request.workspace_id.clone(),
        session_id: request.session_id.clone(),
    };

    let open = |snapshot: Option<Arc<dyn mesh_stream::SnapshotSource>>| {
        mesh_stream::hub().open(mesh_stream::StreamOpen {
            session_id: request.session_id.clone(),
            stream_id: request.stream_id.clone(),
            workspace_id: request.workspace_id.clone(),
            consumer_node_id: consumer_node_id.to_string(),
            consumer_user_id: verified.user_id.clone(),
            window: request.window,
            inline_budget: 0,
            snapshot,
        })
    };

    match kind {
        CodeStudioOwnerStream::Timeline => {
            let (pool, session) = match gate.check_access().await? {
                CodeStudioStreamTarget::Local { pool, session } => (pool, session),
                CodeStudioStreamTarget::Remote { .. } => return Err(CS_END_NOT_FOUND),
            };
            let handle = open(None);
            let after = i64::try_from(request.after_revision).unwrap_or(i64::MAX);
            tokio::spawn(code_studio_timeline_producer(
                handle,
                gate,
                pool,
                after,
                session.status,
            ));
        }
        CodeStudioOwnerStream::Terminal { terminal_id } => {
            match gate.check_access().await? {
                CodeStudioStreamTarget::Local { .. } => {}
                CodeStudioStreamTarget::Remote { .. } => return Err(CS_END_NOT_FOUND),
            }
            let registry = code_studio_terminal_registry(&request.workspace_id)
                .map_err(|_| CS_END_INTERNAL)?;
            // The handle carries the session, so a member of the workspace
            // cannot reach another session's shell by guessing a terminal id.
            let pty = PtyHandle {
                terminal_id,
                session_id: request.session_id.clone(),
            };
            if registry.snapshot(&pty).is_err() {
                return Err(CS_END_TERMINAL_NOT_OPEN);
            }
            let handle = open(Some(Arc::new(CodeStudioTerminalSnapshot {
                registry: Arc::clone(&registry),
                pty: pty.clone(),
            })));
            tokio::spawn(code_studio_terminal_producer(
                handle,
                gate,
                registry,
                pty,
                request.after_revision,
            ));
        }
        CodeStudioOwnerStream::Index => {
            let record = gate.check_workspace().await?;
            if !record.index_enabled {
                return Err(CS_END_INDEX_UNAVAILABLE);
            }
            let handle = open(None);
            tokio::spawn(code_studio_index_producer(
                handle,
                gate,
                request.after_revision,
            ));
        }
    }
    Ok(0)
}

/// Whether a producer should still be producing: the hub holds THIS generation
/// of the stream and nobody has closed it. A reconnect opens a new generation,
/// and the old producer stops here rather than writing into a sequence space
/// that is no longer being read.
fn code_studio_stream_live(handle: &mesh_stream::StreamHandle) -> bool {
    mesh_stream::hub().is_current(handle) && !handle.is_closed()
}

/// Every producer ends the same way: state the reason, leave the buffers long
/// enough for the consumer to collect it, then drop them.
async fn code_studio_finish_owner_stream(
    handle: mesh_stream::StreamHandle,
    reason: &str,
    detail: &str,
) {
    handle.close(reason, detail);
    code_studio_retire(handle).await;
}

/// The stream ended without this producer deciding it — a trust revocation, a
/// session close, or a reconnect that opened a newer generation. The reason is
/// already recorded, so all that is left is to keep the record readable for a
/// moment and then drop the buffers. A generation that is no longer the current
/// one owns nothing and touches nothing.
async fn code_studio_retire(handle: mesh_stream::StreamHandle) {
    if !mesh_stream::hub().is_current(&handle) {
        return;
    }
    tokio::time::sleep(CS_CLOSE_LINGER).await;
    mesh_stream::hub().forget(&handle);
}

/// Publish one frame, or report that the stream ended under us.
async fn code_studio_publish(
    handle: &mesh_stream::StreamHandle,
    revision: u64,
    payload: &CodeStudioPayload,
) -> bool {
    let Ok(bytes) = crate::mesh::cbor::encode(payload) else {
        // A frame this node cannot encode is a bug, not output: the stream says
        // so rather than skipping it and leaving a hole the consumer would read
        // as "nothing happened".
        handle.close(mesh_stream::REASON_ERROR, "a frame could not be encoded");
        return false;
    };
    // `publish` AWAITS credit: a consumer that stops acknowledging slows the
    // producer down instead of growing a buffer on the node that owns
    // everybody's workspaces (§12.2).
    handle
        .publish(mesh_stream::KIND_DATA, revision, bytes)
        .await
        .is_ok()
}

/// Both timeline readers wait the same way, and neither polls.
///
/// The writer announces after it commits (`events::append`); a watermark from
/// `events::append_in_tx`, whose caller commits later, may be ahead of what the
/// log shows, so it is re-read once after a short grace. Every wait is bounded
/// by the revalidation interval, which is what makes an idle session still
/// notice that its reader lost access.
struct CodeStudioTimelineWaiter {
    signal: crate::code_studio::events::EventSignal,
    settled_for: i64,
}

impl CodeStudioTimelineWaiter {
    /// Subscribe BEFORE reading history: an event written between the two
    /// leaves a watermark the reader still sees, so the handover has no gap.
    fn new(session_id: &str) -> Self {
        Self {
            signal: crate::code_studio::events::subscribe(session_id),
            settled_for: 0,
        }
    }

    async fn wait(&mut self, cursor: i64) {
        let announced = self.signal.announced();
        if announced > cursor && self.settled_for != announced {
            // Announced but not visible yet — one re-read, not a poll: a
            // transaction that rolls back leaves `settled_for` equal to the
            // watermark and the next wait goes back to sleeping.
            self.settled_for = announced;
            tokio::time::sleep(crate::code_studio::events::ANNOUNCE_SETTLE).await;
            return;
        }
        let _ = tokio::time::timeout(CS_REVALIDATE_EVERY, self.signal.changed()).await;
    }
}

/// One page of the timeline as wire frames, paired with the sequence each one
/// advances the cursor to. Shared by the local subscription and the mesh
/// producer so the two cannot drift apart.
async fn code_studio_timeline_page(
    pool: &crate::db::DbPool,
    session_id: &str,
    after: i64,
) -> Result<Vec<(i64, CodeStudioPayload)>, ()> {
    let (pool, session) = (pool.clone(), session_id.to_string());
    let page = tokio::task::spawn_blocking(move || {
        crate::code_studio::events::read_after(&pool, &session, after, CS_EVENT_PAGE)
    })
    .await;
    match page {
        Ok(Ok(page)) => Ok(page
            .into_iter()
            .map(|event| {
                let frame = CodeStudioPayload::SessionStreamEvent {
                    seq: event.seq.max(0) as u64,
                    kind: event.kind.slug().to_string(),
                    run_id: event.run_id.clone(),
                    agent_id: event.agent_id.clone(),
                    created_at: event.created_at.clone(),
                    payload_json: code_studio_event_payload_json(&event),
                    security_relevant: event.security_relevant,
                };
                (event.seq, frame)
            })
            .collect()),
        _ => Err(()),
    }
}

/// Owner-side timeline producer. Same log, same cursor rule and same frames as
/// the local subscription — the only difference is where the frame is written.
async fn code_studio_timeline_producer(
    handle: mesh_stream::StreamHandle,
    gate: CodeStudioStreamGate,
    pool: crate::db::DbPool,
    after_seq: i64,
    mut status: String,
) {
    let mut waiter = CodeStudioTimelineWaiter::new(&gate.session_id);
    let mut cursor = after_seq;
    let mut last_check = std::time::Instant::now();
    loop {
        // The status is read BEFORE the drain, so everything the session wrote
        // up to the moment it closed still travels.
        let finished = matches!(status.as_str(), "closed" | "interrupted");
        loop {
            let Ok(page) = code_studio_timeline_page(&pool, &gate.session_id, cursor).await else {
                code_studio_finish_owner_stream(handle, CS_END_INTERNAL, "timeline read failed")
                    .await;
                return;
            };
            let drained = page.len() < CS_EVENT_PAGE;
            for (seq, frame) in page {
                cursor = seq;
                if !code_studio_publish(&handle, seq.max(0) as u64, &frame).await {
                    code_studio_retire(handle).await;
                    return;
                }
            }
            if drained {
                break;
            }
        }
        if finished {
            code_studio_finish_owner_stream(handle, mesh_stream::REASON_SESSION_CLOSED, &status)
                .await;
            return;
        }

        waiter.wait(cursor).await;
        if !code_studio_stream_live(&handle) {
            code_studio_retire(handle).await;
            return;
        }
        if last_check.elapsed() >= CS_REVALIDATE_EVERY {
            last_check = std::time::Instant::now();
            match gate.check_access().await {
                Ok(CodeStudioStreamTarget::Local { session, .. }) => status = session.status,
                Ok(CodeStudioStreamTarget::Remote { .. }) => {
                    code_studio_finish_owner_stream(
                        handle,
                        CS_END_NOT_LOCAL,
                        "the workspace moved to another node",
                    )
                    .await;
                    return;
                }
                Err(reason) => {
                    code_studio_finish_owner_stream(
                        handle,
                        reason,
                        "re-checked against the database",
                    )
                    .await;
                    return;
                }
            }
        }
    }
}

/// Owner-side terminal producer. The VT machine runs here (§7.9), so the frames
/// that travel are finished grid rows and a revision, never raw escape bytes
/// the other node would have to emulate a second time.
async fn code_studio_terminal_producer(
    handle: mesh_stream::StreamHandle,
    gate: CodeStudioStreamGate,
    registry: Arc<TerminalRegistry>,
    pty: PtyHandle,
    after_revision: u64,
) {
    let mut sent = after_revision;
    match registry.snapshot(&pty) {
        Ok(grid) => {
            // A row is only known to be current relative to a revision this
            // node issued, so anything other than the live one earns the whole
            // grid first.
            if sent != grid.revision {
                let frame = CodeStudioPayload::TerminalStreamSnapshot {
                    revision: grid.revision,
                    grid_rows: grid.rows,
                    grid_cols: grid.cols,
                    cursor: code_studio_cursor(grid.cursor),
                    rows: code_studio_cell_rows(&grid.lines),
                };
                if !code_studio_publish(&handle, grid.revision, &frame).await {
                    code_studio_retire(handle).await;
                    return;
                }
                sent = grid.revision;
            }
        }
        Err(_) => {
            code_studio_finish_owner_stream(handle, CS_END_TERMINAL_NOT_OPEN, "").await;
            return;
        }
    }

    let mut last_check = std::time::Instant::now();
    loop {
        tokio::time::sleep(CS_TERMINAL_POLL).await;
        if !code_studio_stream_live(&handle) {
            code_studio_retire(handle).await;
            return;
        }
        let Ok(changes) = registry.changes_since(&pty, sent) else {
            code_studio_finish_owner_stream(handle, CS_END_TERMINAL_NOT_OPEN, "").await;
            return;
        };
        if changes.revision != sent {
            let frame = CodeStudioPayload::TerminalStreamDelta {
                revision: changes.revision,
                grid_rows: changes.rows,
                grid_cols: changes.cols,
                cursor: code_studio_cursor(changes.cursor),
                rows: code_studio_cell_rows(&changes.lines),
            };
            if !code_studio_publish(&handle, changes.revision, &frame).await {
                code_studio_retire(handle).await;
                return;
            }
            sent = changes.revision;
        }
        // Checked AFTER the delta, so the shell's last output reaches the
        // consumer before the stream reports that it ended.
        match registry.state(&pty) {
            Ok(TerminalState::Running) => {}
            Ok(_) => {
                code_studio_finish_owner_stream(handle, CS_END_TERMINAL_EXITED, "").await;
                return;
            }
            Err(_) => {
                code_studio_finish_owner_stream(handle, CS_END_TERMINAL_NOT_OPEN, "").await;
                return;
            }
        }
        if last_check.elapsed() >= CS_REVALIDATE_EVERY {
            last_check = std::time::Instant::now();
            match gate.check_access().await {
                Ok(CodeStudioStreamTarget::Local { .. }) => {}
                Ok(CodeStudioStreamTarget::Remote { .. }) => {
                    code_studio_finish_owner_stream(
                        handle,
                        CS_END_NOT_LOCAL,
                        "the workspace moved to another node",
                    )
                    .await;
                    return;
                }
                Err(reason) => {
                    code_studio_finish_owner_stream(
                        handle,
                        reason,
                        "re-checked against the database",
                    )
                    .await;
                    return;
                }
            }
        }
    }
}

/// Owner-side index producer, fed by the indexer's own progress channel — the
/// same subscription the local stream uses, so a job publishes once and both
/// readers see it.
async fn code_studio_index_producer(
    handle: mesh_stream::StreamHandle,
    gate: CodeStudioStreamGate,
    after_seq: u64,
) {
    // Subscribed BEFORE the history is read: a frame published between the two
    // would otherwise fall between the replay and the tail.
    let mut live = crate::code_studio::index::subscribe_progress(&gate.workspace_id);
    let mut cursor = after_seq;
    for frame in crate::code_studio::index::progress_since(&gate.workspace_id, cursor) {
        cursor = frame.seq;
        if !code_studio_publish(&handle, cursor, &code_studio_index_frame(frame)).await {
            code_studio_retire(handle).await;
            return;
        }
    }

    let mut last_check = std::time::Instant::now();
    loop {
        match tokio::time::timeout(CS_REVALIDATE_EVERY, live.recv()).await {
            Ok(Ok(frame)) => {
                if frame.seq > cursor {
                    cursor = frame.seq;
                    if !code_studio_publish(&handle, cursor, &code_studio_index_frame(frame)).await
                    {
                        code_studio_retire(handle).await;
                        return;
                    }
                }
            }
            // The producer fell behind the broadcast channel. Nothing is
            // skipped silently: the indexer's bounded history is replayed from
            // the cursor, and each record is cumulative.
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                for frame in crate::code_studio::index::progress_since(&gate.workspace_id, cursor) {
                    cursor = frame.seq;
                    if !code_studio_publish(&handle, cursor, &code_studio_index_frame(frame)).await
                    {
                        code_studio_retire(handle).await;
                        return;
                    }
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                code_studio_finish_owner_stream(handle, CS_END_INDEX_UNAVAILABLE, "").await;
                return;
            }
            Err(_elapsed) => {}
        }
        if !code_studio_stream_live(&handle) {
            code_studio_retire(handle).await;
            return;
        }
        if last_check.elapsed() >= CS_REVALIDATE_EVERY {
            last_check = std::time::Instant::now();
            match gate.check_workspace().await {
                Ok(record) if record.index_enabled => {}
                Ok(_) => {
                    code_studio_finish_owner_stream(handle, CS_END_INDEX_UNAVAILABLE, "").await;
                    return;
                }
                Err(reason) => {
                    code_studio_finish_owner_stream(
                        handle,
                        reason,
                        "re-checked against the database",
                    )
                    .await;
                    return;
                }
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Remote workspaces: the frames are produced on the owner node (§12.2)
// -----------------------------------------------------------------------------

/// The streams a remote workspace serves over the mesh. All three have a
/// producer on the owner node, so all three can be pulled.
#[derive(Clone, Copy)]
enum CodeStudioRemoteStream {
    Timeline,
    Terminal,
    Index,
}

impl CodeStudioRemoteStream {
    /// Frames this stream may carry. The owner node is trusted to authorize,
    /// not to decide what the browser renders: any other variant is drift
    /// between two builds, and forwarding it would push a payload the UI has no
    /// rule for into a live subscription.
    fn accepts(self, frame: &CodeStudioPayload) -> bool {
        matches!(
            (self, frame),
            (Self::Timeline, CodeStudioPayload::SessionStreamEvent { .. })
                | (
                    Self::Terminal,
                    CodeStudioPayload::TerminalStreamSnapshot { .. }
                        | CodeStudioPayload::TerminalStreamDelta { .. }
                )
                | (Self::Index, CodeStudioPayload::IndexStreamProgress { .. })
        )
    }
}

/// Whether a frame pulled from the owner still interests a client that already
/// holds everything up to `after`.
///
/// The hub's sequence numbers and the protocol's own `seq`/`revision` are
/// different spaces (§12.2): the hub cursor deduplicates the TRANSPORT, this
/// deduplicates what the browser asked to resume from. A snapshot is compared
/// for equality, not order, exactly as the local terminal path does — a
/// snapshot at the revision the client holds is the screen it already shows.
fn code_studio_remote_frame_is_new(frame: &CodeStudioPayload, after: u64) -> bool {
    match frame {
        CodeStudioPayload::SessionStreamEvent { seq, .. } => *seq > after,
        CodeStudioPayload::TerminalStreamDelta { revision, .. } => *revision > after,
        CodeStudioPayload::TerminalStreamSnapshot { revision, .. } => *revision != after,
        CodeStudioPayload::IndexStreamProgress { seq, .. } => *seq > after,
        _ => true,
    }
}

/// What a refusal from the owner node means for the browser.
///
/// The mapping is deliberately coarse: `NotFound` is the ONE answer the owner
/// gives for a session that does not exist, a session belonging to somebody
/// else and a workspace it does not run, so nothing here can pull those apart
/// either.
fn code_studio_denied_reason(error: &tentaflow_protocol::ProtocolError) -> String {
    use tentaflow_protocol::ProtocolErrorCode;
    match error.code {
        ProtocolErrorCode::NotFound => CS_END_NOT_FOUND,
        ProtocolErrorCode::PolicyDenied | ProtocolErrorCode::AuthRequired => {
            CS_END_PERMISSION_REVOKED
        }
        ProtocolErrorCode::Internal => CS_END_INTERNAL,
        _ => CS_END_OWNER_UNREACHABLE,
    }
    .to_string()
}

/// Reason to end the local subscription with when a stream call failed.
fn code_studio_stream_error_reason(error: &mesh_stream::StreamError) -> String {
    match error {
        mesh_stream::StreamError::Denied(denied) => code_studio_denied_reason(denied),
        _ => CS_END_OWNER_UNREACHABLE.to_string(),
    }
}

/// Turn one pulled batch into the frames the local subscription should carry
/// and, when the stream is over, the reason to end it with.
///
/// The frames travel as the CBOR of the same `CodeStudioPayload` the local
/// path emits — one encoding for the whole system, the one `remote_proxy`
/// already uses for forwarded requests. Frames decoded before a bad one are
/// kept: they were produced and paid for, and dropping them would lose output
/// the client cannot ask for again.
fn code_studio_decode_batch(
    kind: CodeStudioRemoteStream,
    batch: mesh_stream::ConsumedBatch,
    after: u64,
) -> (Vec<CodeStudioPayload>, Option<String>) {
    let mut frames = Vec::with_capacity(batch.frames.len());
    for frame in batch.frames {
        if frame.kind == mesh_stream::KIND_ARTIFACT {
            return (frames, Some(CS_END_STREAM_OVERFLOW.to_string()));
        }
        let payload: CodeStudioPayload = match crate::mesh::cbor::decode(&frame.data) {
            Ok(payload) => payload,
            Err(e) => {
                tracing::warn!(
                    stream = %frame.stream_id,
                    "code studio: undecodable frame from the owner node: {e}"
                );
                return (frames, Some(CS_END_INTERNAL.to_string()));
            }
        };
        if !kind.accepts(&payload) {
            tracing::warn!(
                stream = %frame.stream_id,
                "code studio: the owner node sent a frame that does not belong to this stream"
            );
            return (frames, Some(CS_END_INTERNAL.to_string()));
        }
        if !code_studio_remote_frame_is_new(&payload, after) {
            continue;
        }
        frames.push(payload);
    }
    // The owner's reason IS the reason. It comes from the same vocabulary
    // (`session_closed`, `trust_lost`, `gap`, …), and rewriting it here would
    // replace what happened with a guess.
    (frames, batch.close.map(|close| close.reason))
}

/// Consume a stream produced on the workspace's owner node (§12.2).
///
/// The dashboard node has no decision logic (§3): it pulls frames, keeps
/// checking that this person may still read this workspace, and pushes what it
/// receives into the local subscription. The frames travel as the CBOR of the
/// same `CodeStudioPayload` the local path emits — one encoding for the whole
/// system, the one `remote_proxy` already uses for forwarded requests.
///
/// Returns the reason to end the subscription with, or `None` when the browser
/// went away and there is nobody left to tell.
async fn code_studio_pull_from_owner(
    sub: &Arc<Subscription>,
    gate: &CodeStudioStreamGate,
    iroh: Option<Arc<IrohMeshManager>>,
    owner_node_id: String,
    kind: CodeStudioRemoteStream,
    stream_id: String,
    after: u64,
    poll: std::time::Duration,
) -> Option<String> {
    let Some(iroh) = iroh else {
        return Some(CS_END_OWNER_UNREACHABLE.to_string());
    };
    // A stream carries somebody's source code and terminal. It is pulled only
    // from a peer this node has trust-paired with, never from a node that
    // merely claims to own the workspace.
    if !iroh.is_trusted(&owner_node_id) {
        return Some(CS_END_OWNER_UNREACHABLE.to_string());
    }

    let mut remote = mesh_stream::RemoteStream {
        iroh: Arc::clone(&iroh),
        owner_node_id: owner_node_id.clone(),
        workspace_id: gate.workspace_id.clone(),
        session_id: gate.session_id.clone(),
        stream_id: stream_id.clone(),
        db: gate.db.clone(),
        local_node_id: gate.node_id.clone(),
        user_id: gate.user_id.clone(),
        org_id: gate.org_id.clone(),
        cursor: mesh_stream::StreamCursor::default(),
    };
    // Nothing is produced on the owner node until it has authorized the PERSON
    // this subscription belongs to (§12.1). The resume point travels with the
    // open, so the producer starts where the browser stopped.
    if let Err(e) = remote
        .open(after, CS_PULL_CREDITS, CS_PULL_TIMEOUT_SECS)
        .await
    {
        tracing::debug!(
            owner = %owner_node_id,
            stream = %stream_id,
            "code studio: the owner node did not open the stream: {e}"
        );
        return Some(code_studio_stream_error_reason(&e));
    }

    let mut last_check = std::time::Instant::now();
    loop {
        let batch = match remote.pull(CS_PULL_CREDITS, CS_PULL_TIMEOUT_SECS).await {
            Ok(batch) => batch,
            Err(e) => {
                tracing::warn!(
                    owner = %owner_node_id,
                    session = %gate.session_id,
                    stream = %stream_id,
                    "code studio: pulling a remote stream failed: {e}"
                );
                return Some(code_studio_stream_error_reason(&e));
            }
        };

        let had_frames = !batch.frames.is_empty();
        let (frames, closed) = code_studio_decode_batch(kind, batch, after);
        for payload in frames {
            // The credit window, same as the local path: awaits a free slot.
            if push_chunk_async(sub, MessageBody::CodeStudioBody(payload))
                .await
                .is_err()
            {
                return None;
            }
        }
        if let Some(reason) = closed {
            return Some(reason);
        }

        // A pull reads a buffer and returns at once, so an empty answer is the
        // only thing worth waiting on. A batch that carried frames is followed
        // immediately, which is what keeps the keystroke echo inside its SLO.
        if !had_frames {
            tokio::time::sleep(poll).await;
        }
        if sub.tx.is_closed() {
            return None;
        }
        if last_check.elapsed() >= CS_REVALIDATE_EVERY {
            last_check = std::time::Instant::now();
            // Re-read from THIS node's database, not from the connection's
            // snapshot: a proxy that stopped checking would let a revoked
            // membership keep reading for as long as the socket lived. The
            // owner node re-checks its own half on every call.
            if let Err(reason) = gate.check_remote_owner(&owner_node_id).await {
                return Some(reason.to_string());
            }
            if !iroh.is_trusted(&owner_node_id) {
                return Some(mesh_stream::REASON_TRUST_LOST.to_string());
            }
        }
    }
}

/// The event body as the UI reads it. Above the frame budget the body is NOT
/// truncated into the frame: it is already in the artifact store, redacted
/// (§13.2, §13.4), and the frame says where.
fn code_studio_event_payload_json(event: &crate::code_studio::events::StoredEvent) -> String {
    match serde_json::to_string(&event.payload) {
        Ok(body) if body.len() <= CS_MAX_EVENT_PAYLOAD_BYTES => body,
        Ok(body) => serde_json::json!({
            "oversized": true,
            "bytes": body.len(),
            "artifact_ref": event.artifact_ref,
        })
        .to_string(),
        Err(e) => serde_json::json!({ "unrenderable": e.to_string() }).to_string(),
    }
}

/// Packs one cell's colours and style into the single word `TerminalCellRow`
/// carries per character:
///
/// * bits 0..7 — style flags, the exact bit layout of `terminal::attrs`
/// * bits 8..15 — foreground colour index
/// * bits 16..23 — background colour index
/// * bit 24 — the foreground is set (clear means the theme's default)
/// * bit 25 — the background is set
///
/// A true-colour sequence is quantised to the xterm-256 palette. Two colours
/// plus the flags do not fit in 32 bits any other way, and one word per
/// character is the row format the wire already fixed.
fn code_studio_pack_attrs(cell: &Cell) -> u32 {
    let mut word = u32::from(cell.attrs & 0x00ff);
    if let Some(index) = code_studio_color_index(cell.fg) {
        word |= u32::from(index) << 8;
        word |= 1 << 24;
    }
    if let Some(index) = code_studio_color_index(cell.bg) {
        word |= u32::from(index) << 16;
        word |= 1 << 25;
    }
    word
}

fn code_studio_color_index(color: Color) -> Option<u8> {
    match color {
        Color::Default => None,
        Color::Indexed(index) => Some(index),
        Color::Rgb(r, g, b) => Some(code_studio_quantize_rgb(r, g, b)),
    }
}

/// Nearest xterm-256 entry: the 6×6×6 colour cube or the 24-step grey ramp,
/// whichever is closer. The ramp matters — a neutral grey run through the cube
/// alone would visibly band.
fn code_studio_quantize_rgb(r: u8, g: u8, b: u8) -> u8 {
    const CUBE: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let nearest = |v: u8| -> usize {
        CUBE.iter()
            .enumerate()
            .min_by_key(|(_, level)| (**level - i32::from(v)).abs())
            .map(|(i, _)| i)
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (nearest(r), nearest(g), nearest(b));
    let cube_cost = (CUBE[ri] - i32::from(r)).abs()
        + (CUBE[gi] - i32::from(g)).abs()
        + (CUBE[bi] - i32::from(b)).abs();

    let average = (i32::from(r) + i32::from(g) + i32::from(b)) / 3;
    let grey_step = ((average - 8 + 5) / 10).clamp(0, 23);
    let grey_value = 8 + grey_step * 10;
    let grey_cost = (grey_value - i32::from(r)).abs()
        + (grey_value - i32::from(g)).abs()
        + (grey_value - i32::from(b)).abs();

    if grey_cost < cube_cost {
        (232 + grey_step) as u8
    } else {
        (16 + 36 * ri + 6 * gi + bi) as u8
    }
}

/// Grid rows in wire form. The trailing half of a double-width character is a
/// placeholder in the grid, not a character: it is dropped, so `attrs` has one
/// word per character of `text` and a renderer measures width itself.
fn code_studio_cell_rows(lines: &[GridRow]) -> Vec<TerminalCellRow> {
    lines
        .iter()
        .map(|line| {
            let mut text = String::with_capacity(line.cells.len());
            let mut attrs = Vec::with_capacity(line.cells.len());
            for cell in &line.cells {
                if cell.ch == '\0' {
                    continue;
                }
                text.push(cell.ch);
                attrs.push(code_studio_pack_attrs(cell));
            }
            TerminalCellRow {
                row: u32::from(line.index),
                text,
                attrs,
            }
        })
        .collect()
}

fn code_studio_cursor(cursor: Cursor) -> TerminalCursorInfo {
    TerminalCursorInfo {
        row: cursor.row,
        col: cursor.col,
        visible: cursor.visible,
    }
}

/// Live session timeline. History comes from the event log by cursor and the
/// tail keeps reading the same log, so a reconnect at `after_seq` resumes
/// exactly where it stopped: the log is written in the same transaction as the
/// state change it records (§13.3), which is what makes "no gap, no duplicate"
/// a property of the storage rather than of a buffer.
fn code_studio_session_stream_handler(
    req: MessageBody,
    ctx: HandlerContext,
    sub: Arc<Subscription>,
) {
    let (workspace_id, session_id, after_seq) = match req {
        MessageBody::CodeStudioBody(CodeStudioPayload::SessionStreamRequest {
            workspace_id,
            session_id,
            after_seq,
        }) => (workspace_id, session_id, after_seq),
        _ => {
            let _ = push_end(&sub, None);
            return;
        }
    };
    let Some(org) = ctx.org_context.as_ref() else {
        let _ = push_end(
            &sub,
            Some(MessageBody::CodeStudioBody(
                CodeStudioPayload::SessionStreamEnd {
                    reason: CS_END_NOT_FOUND.to_string(),
                },
            )),
        );
        return;
    };

    let gate = CodeStudioStreamGate {
        db: ctx.state.db.clone(),
        node_id: ctx.state.local_node_id.to_string(),
        user_id: org.user_id.clone(),
        org_id: org.org_id.clone(),
        workspace_id,
        session_id: session_id.clone(),
    };
    let iroh = ctx.state.quic_mesh.clone();

    tokio::spawn(async move {
        let end = |reason: &str| CodeStudioPayload::SessionStreamEnd {
            reason: reason.to_string(),
        };
        let (pool, session) = match gate.check_access().await {
            Ok(CodeStudioStreamTarget::Local { pool, session }) => (pool, session),
            Ok(CodeStudioStreamTarget::Remote { owner_node_id }) => {
                // §12.2 — the timeline is written where the session runs, so it
                // is pulled from there. The browser's `after_seq` still means
                // the event log's sequence, and it is applied to the frames.
                if let Some(reason) = code_studio_pull_from_owner(
                    &sub,
                    &gate,
                    iroh,
                    owner_node_id,
                    CodeStudioRemoteStream::Timeline,
                    CS_STREAM_TIMELINE.to_string(),
                    after_seq,
                    CS_SESSION_POLL,
                )
                .await
                {
                    code_studio_end(&sub, end(&reason)).await;
                }
                return;
            }
            Err(reason) => {
                code_studio_end(&sub, end(reason)).await;
                return;
            }
        };

        // A cursor beyond i64 cannot name a row; it resumes past the end of the
        // log rather than wrapping into a replay.
        let mut cursor = i64::try_from(after_seq).unwrap_or(i64::MAX);
        let mut status = session.status;
        let mut last_check = std::time::Instant::now();
        // Subscribed before the history is read, so an event written during the
        // catch-up cannot fall between the replay and the tail.
        let mut waiter = CodeStudioTimelineWaiter::new(&session_id);

        loop {
            // The status is read BEFORE the drain, so everything the session
            // wrote up to the moment it closed is still delivered.
            let finished = matches!(status.as_str(), "closed" | "interrupted");

            loop {
                let Ok(page) = code_studio_timeline_page(&pool, &session_id, cursor).await else {
                    code_studio_end(&sub, end(CS_END_INTERNAL)).await;
                    return;
                };
                let drained = page.len() < CS_EVENT_PAGE;
                for (seq, frame) in page {
                    // Monotonic: the next read asks for `seq > cursor`, so the
                    // same event can never be sent twice.
                    cursor = seq;
                    // The credit window: awaits a free slot instead of queuing.
                    if push_chunk_async(&sub, MessageBody::CodeStudioBody(frame))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                if drained {
                    break;
                }
            }

            if finished {
                code_studio_end(&sub, end(CS_END_SESSION_CLOSED)).await;
                return;
            }

            // The writer wakes this loop (§13.3 — the log is the source of
            // truth and its author says when it grew), so an idle session costs
            // one query per revalidation instead of one per interval.
            waiter.wait(cursor).await;
            if sub.tx.is_closed() {
                return;
            }
            if last_check.elapsed() >= CS_REVALIDATE_EVERY {
                last_check = std::time::Instant::now();
                match gate.check_access().await {
                    Ok(CodeStudioStreamTarget::Local { session, .. }) => status = session.status,
                    // The workspace moved to another node mid-stream. The log
                    // this loop is reading stopped being the live one, so the
                    // stream ends and the client resubscribes — onto the remote
                    // path this time.
                    Ok(CodeStudioStreamTarget::Remote { .. }) => {
                        code_studio_end(&sub, end(CS_END_NOT_LOCAL)).await;
                        return;
                    }
                    Err(reason) => {
                        code_studio_end(&sub, end(reason)).await;
                        return;
                    }
                }
            }
        }
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "CodeStudioSessionStreamRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: code_studio_session_stream_handler,
    }
}

/// Live terminal grid. The client sends the revision it holds; anything else
/// than the live one earns a full snapshot first, because a row is only known
/// to be current relative to a revision the server issued.
fn code_studio_terminal_stream_handler(
    req: MessageBody,
    ctx: HandlerContext,
    sub: Arc<Subscription>,
) {
    let (workspace_id, session_id, terminal_id, after_revision) = match req {
        MessageBody::CodeStudioBody(CodeStudioPayload::TerminalStreamRequest {
            workspace_id,
            session_id,
            terminal_id,
            after_revision,
        }) => (workspace_id, session_id, terminal_id, after_revision),
        _ => {
            let _ = push_end(&sub, None);
            return;
        }
    };
    let Some(org) = ctx.org_context.as_ref() else {
        let _ = push_end(
            &sub,
            Some(MessageBody::CodeStudioBody(
                CodeStudioPayload::TerminalStreamEnd {
                    reason: CS_END_NOT_FOUND.to_string(),
                },
            )),
        );
        return;
    };

    let gate = CodeStudioStreamGate {
        db: ctx.state.db.clone(),
        node_id: ctx.state.local_node_id.to_string(),
        user_id: org.user_id.clone(),
        org_id: org.org_id.clone(),
        workspace_id: workspace_id.clone(),
        session_id: session_id.clone(),
    };
    let iroh = ctx.state.quic_mesh.clone();

    tokio::spawn(async move {
        let end = |reason: &str| CodeStudioPayload::TerminalStreamEnd {
            reason: reason.to_string(),
        };
        match gate.check_access().await {
            Ok(CodeStudioStreamTarget::Local { .. }) => {}
            Ok(CodeStudioStreamTarget::Remote { owner_node_id }) => {
                // The VT grid is parsed where the shell runs (§7.9), so this
                // node pulls finished frames instead of raw bytes it would have
                // to emulate a second time.
                if let Some(reason) = code_studio_pull_from_owner(
                    &sub,
                    &gate,
                    iroh,
                    owner_node_id,
                    CodeStudioRemoteStream::Terminal,
                    code_studio_terminal_stream_id(&terminal_id),
                    after_revision,
                    CS_REMOTE_TERMINAL_POLL,
                )
                .await
                {
                    code_studio_end(&sub, end(&reason)).await;
                }
                return;
            }
            Err(reason) => {
                code_studio_end(&sub, end(reason)).await;
                return;
            }
        }
        let Ok(registry) = code_studio_terminal_registry(&workspace_id) else {
            code_studio_end(&sub, end(CS_END_INTERNAL)).await;
            return;
        };
        // The handle carries the session, so a member cannot reach another
        // session's terminal by guessing an id.
        let handle = PtyHandle {
            terminal_id,
            session_id,
        };

        let mut sent = after_revision;
        match registry.snapshot(&handle) {
            Ok(grid) => {
                if sent != grid.revision {
                    let frame = CodeStudioPayload::TerminalStreamSnapshot {
                        revision: grid.revision,
                        grid_rows: grid.rows,
                        grid_cols: grid.cols,
                        cursor: code_studio_cursor(grid.cursor),
                        rows: code_studio_cell_rows(&grid.lines),
                    };
                    if push_chunk_async(&sub, MessageBody::CodeStudioBody(frame))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    sent = grid.revision;
                }
            }
            Err(_) => {
                code_studio_end(&sub, end(CS_END_TERMINAL_NOT_OPEN)).await;
                return;
            }
        }

        let mut last_check = std::time::Instant::now();
        loop {
            tokio::time::sleep(CS_TERMINAL_POLL).await;
            if sub.tx.is_closed() {
                return;
            }
            let Ok(changes) = registry.changes_since(&handle, sent) else {
                code_studio_end(&sub, end(CS_END_TERMINAL_NOT_OPEN)).await;
                return;
            };
            if changes.revision != sent {
                let frame = CodeStudioPayload::TerminalStreamDelta {
                    revision: changes.revision,
                    grid_rows: changes.rows,
                    grid_cols: changes.cols,
                    cursor: code_studio_cursor(changes.cursor),
                    rows: code_studio_cell_rows(&changes.lines),
                };
                if push_chunk_async(&sub, MessageBody::CodeStudioBody(frame))
                    .await
                    .is_err()
                {
                    return;
                }
                sent = changes.revision;
            }
            // Checked AFTER the delta above, so the shell's last output reaches
            // the client before the stream reports that it ended.
            match registry.state(&handle) {
                Ok(TerminalState::Running) => {}
                Ok(_) => {
                    code_studio_end(&sub, end(CS_END_TERMINAL_EXITED)).await;
                    return;
                }
                Err(_) => {
                    code_studio_end(&sub, end(CS_END_TERMINAL_NOT_OPEN)).await;
                    return;
                }
            }
            if last_check.elapsed() >= CS_REVALIDATE_EVERY {
                last_check = std::time::Instant::now();
                match gate.check_access().await {
                    Ok(CodeStudioStreamTarget::Local { .. }) => {}
                    // The grid this loop reads belongs to a shell on a node the
                    // workspace no longer runs on.
                    Ok(CodeStudioStreamTarget::Remote { .. }) => {
                        code_studio_end(&sub, end(CS_END_NOT_LOCAL)).await;
                        return;
                    }
                    Err(reason) => {
                        code_studio_end(&sub, end(reason)).await;
                        return;
                    }
                }
            }
        }
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "CodeStudioTerminalStreamRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: code_studio_terminal_stream_handler,
    }
}

/// One indexer progress record as its wire frame, field for field.
fn code_studio_index_frame(frame: crate::code_studio::index::IndexProgress) -> CodeStudioPayload {
    CodeStudioPayload::IndexStreamProgress {
        seq: frame.seq,
        job_id: frame.job_id,
        workspace_id: frame.workspace_id,
        branch: frame.branch,
        phase: frame.phase,
        files_done: frame.files_done,
        files_total: frame.files_total,
        chunks: frame.chunks,
        message: frame.message,
        terminal: frame.terminal,
    }
}

/// Indexing progress (§14). History from `after_seq` first, then the live
/// channel: the indexer keeps a bounded per-workspace log, so a client that
/// reconnects with its cursor gets what it missed instead of starting over.
///
/// A `terminal` frame ends a JOB, not this subscription — the workspace stays
/// indexable and the next job publishes onto the same stream, which is what the
/// wire contract says (`IndexStreamProgress.terminal`). The subscription ends
/// when access ends, when the index is switched off, or when the client leaves.
fn code_studio_index_stream_handler(req: MessageBody, ctx: HandlerContext, sub: Arc<Subscription>) {
    let (workspace_id, after_seq) = match req {
        MessageBody::CodeStudioBody(CodeStudioPayload::IndexStreamRequest {
            workspace_id,
            after_seq,
        }) => (workspace_id, after_seq),
        _ => {
            let _ = push_end(&sub, None);
            return;
        }
    };
    let Some(org) = ctx.org_context.as_ref() else {
        let _ = push_end(
            &sub,
            Some(MessageBody::CodeStudioBody(
                CodeStudioPayload::IndexStreamEnd {
                    reason: CS_END_NOT_FOUND.to_string(),
                },
            )),
        );
        return;
    };

    let gate = CodeStudioStreamGate {
        db: ctx.state.db.clone(),
        node_id: ctx.state.local_node_id.to_string(),
        user_id: org.user_id.clone(),
        org_id: org.org_id.clone(),
        workspace_id,
        session_id: String::new(),
    };
    let iroh = ctx.state.quic_mesh.clone();

    tokio::spawn(async move {
        let end = |reason: &str| CodeStudioPayload::IndexStreamEnd {
            reason: reason.to_string(),
        };
        match gate.check_workspace().await {
            Ok(record) if record.index_enabled => {}
            Ok(_) => {
                code_studio_end(&sub, end(CS_END_INDEX_UNAVAILABLE)).await;
                return;
            }
            // The indexer runs where the repository is (§14), so its progress
            // is pulled from the owner node like every other stream.
            Err(CS_END_NOT_LOCAL) => {
                let owner = match gate.check_owner_node().await {
                    Ok(owner) => owner,
                    Err(reason) => {
                        code_studio_end(&sub, end(reason)).await;
                        return;
                    }
                };
                let stream_id = code_studio_index_stream_id(&gate.workspace_id);
                if let Some(reason) = code_studio_pull_from_owner(
                    &sub,
                    &gate,
                    iroh,
                    owner,
                    CodeStudioRemoteStream::Index,
                    stream_id,
                    after_seq,
                    CS_SESSION_POLL,
                )
                .await
                {
                    code_studio_end(&sub, end(&reason)).await;
                }
                return;
            }
            Err(reason) => {
                code_studio_end(&sub, end(reason)).await;
                return;
            }
        }

        // Subscribed BEFORE the history is read: a frame published between the
        // two would otherwise fall between the replay and the tail, and the
        // client would never learn it existed.
        let mut live = crate::code_studio::index::subscribe_progress(&gate.workspace_id);
        let mut cursor = after_seq;
        for frame in crate::code_studio::index::progress_since(&gate.workspace_id, cursor) {
            cursor = frame.seq;
            if push_chunk_async(
                &sub,
                MessageBody::CodeStudioBody(code_studio_index_frame(frame)),
            )
            .await
            .is_err()
            {
                return;
            }
        }

        let mut last_check = std::time::Instant::now();
        loop {
            // The timeout is what makes the gate below run on an idle stream:
            // a workspace can go a whole day without an indexing job, and a
            // membership revoked in that time must still end the subscription.
            match tokio::time::timeout(CS_REVALIDATE_EVERY, live.recv()).await {
                Ok(Ok(frame)) => {
                    if frame.seq > cursor {
                        cursor = frame.seq;
                        if push_chunk_async(
                            &sub,
                            MessageBody::CodeStudioBody(code_studio_index_frame(frame)),
                        )
                        .await
                        .is_err()
                        {
                            return;
                        }
                    }
                }
                // The consumer fell behind the broadcast channel. Nothing is
                // skipped silently: the indexer's bounded history is replayed
                // from the cursor, and each progress record is cumulative, so
                // the newest one restates whatever an older one would have said.
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                    for frame in
                        crate::code_studio::index::progress_since(&gate.workspace_id, cursor)
                    {
                        cursor = frame.seq;
                        if push_chunk_async(
                            &sub,
                            MessageBody::CodeStudioBody(code_studio_index_frame(frame)),
                        )
                        .await
                        .is_err()
                        {
                            return;
                        }
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                    code_studio_end(&sub, end(CS_END_INDEX_UNAVAILABLE)).await;
                    return;
                }
                Err(_elapsed) => {}
            }
            if sub.tx.is_closed() {
                return;
            }
            if last_check.elapsed() >= CS_REVALIDATE_EVERY {
                last_check = std::time::Instant::now();
                match gate.check_workspace().await {
                    Ok(record) if record.index_enabled => {}
                    Ok(_) => {
                        code_studio_end(&sub, end(CS_END_INDEX_UNAVAILABLE)).await;
                        return;
                    }
                    Err(reason) => {
                        code_studio_end(&sub, end(reason)).await;
                        return;
                    }
                }
            }
        }
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "CodeStudioIndexStreamRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: code_studio_index_stream_handler,
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
            connection_id: 0,
            resume_secret: Some(secret),
            state: super::super::state::AppState::for_test(),
            org_context: None,
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
            connection_id: 0,
            resume_secret: Some(Arc::new(b"test-secret".to_vec())),
            state: super::super::state::AppState::for_test(),
            org_context: None,
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
            connection_id: 0,
            resume_secret: Some(secret),
            state: super::super::state::AppState::for_test(),
            org_context: None,
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
                reasoning_content: None,
            }],
            temperature: None,
            max_tokens: None,
            flow_id: None,
            session_id: None,
        });
        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: super::super::state::AppState::for_test(),
            org_context: None,
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

    // ---- Code Studio streams (§12.2) ----

    fn code_studio_test_ctx() -> super::super::HandlerContext {
        super::super::HandlerContext {
            session: tentaflow_protocol::SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: super::super::state::AppState::for_test(),
            org_context: None,
        }
    }

    #[test]
    fn code_studio_stream_handlers_are_registered() {
        for variant in [
            "CodeStudioSessionStreamRequest",
            "CodeStudioTerminalStreamRequest",
            "CodeStudioIndexStreamRequest",
        ] {
            let handler =
                find_stream_handler(variant).unwrap_or_else(|| panic!("{variant} not registered"));
            assert_eq!(handler.required_auth, SessionAuthKind::UserSession);
        }
    }

    /// Without an org context there is nobody whose membership could be checked,
    /// and the answer has to be the same uniform denial a stranger gets — with a
    /// NAMED reason, because a silent end renders in the UI as "no events yet".
    #[tokio::test]
    async fn a_code_studio_stream_without_an_org_context_denies_uniformly() {
        use super::super::subscription::SubscriptionRegistry;
        use tentaflow_protocol::code_studio::CodeStudioPayload;
        use tentaflow_protocol::MessageBody;

        let registry = SubscriptionRegistry::new();

        let (sub, mut rx) = registry.create(1, None);
        (find_stream_handler("CodeStudioSessionStreamRequest")
            .unwrap()
            .handler_fn)(
            MessageBody::CodeStudioBody(CodeStudioPayload::SessionStreamRequest {
                workspace_id: "w1".into(),
                session_id: "s1".into(),
                after_seq: 0,
            }),
            code_studio_test_ctx(),
            sub,
        );
        match rx.recv().await {
            Some(SubscriptionEvent::End(Some(MessageBody::CodeStudioBody(
                CodeStudioPayload::SessionStreamEnd { reason },
            )))) => assert_eq!(reason, "not_found"),
            other => panic!("expected a named session end, got {other:?}"),
        }

        let (sub, mut rx) = registry.create(2, None);
        (find_stream_handler("CodeStudioTerminalStreamRequest")
            .unwrap()
            .handler_fn)(
            MessageBody::CodeStudioBody(CodeStudioPayload::TerminalStreamRequest {
                workspace_id: "w1".into(),
                session_id: "s1".into(),
                terminal_id: "t1".into(),
                after_revision: 0,
            }),
            code_studio_test_ctx(),
            sub,
        );
        match rx.recv().await {
            Some(SubscriptionEvent::End(Some(MessageBody::CodeStudioBody(
                CodeStudioPayload::TerminalStreamEnd { reason },
            )))) => assert_eq!(reason, "not_found"),
            other => panic!("expected a named terminal end, got {other:?}"),
        }

        let (sub, mut rx) = registry.create(3, None);
        (find_stream_handler("CodeStudioIndexStreamRequest")
            .unwrap()
            .handler_fn)(
            MessageBody::CodeStudioBody(CodeStudioPayload::IndexStreamRequest {
                workspace_id: "w1".into(),
                after_seq: 0,
            }),
            code_studio_test_ctx(),
            sub,
        );
        match rx.recv().await {
            Some(SubscriptionEvent::End(Some(MessageBody::CodeStudioBody(
                CodeStudioPayload::IndexStreamEnd { reason },
            )))) => assert_eq!(reason, "not_found"),
            other => panic!("expected a named index end, got {other:?}"),
        }
    }

    /// A body that is not this stream's request ends the subscription instead of
    /// leaving the client's slot open forever.
    #[tokio::test]
    async fn a_code_studio_stream_refuses_a_body_that_is_not_its_request() {
        use super::super::subscription::SubscriptionRegistry;
        use tentaflow_protocol::MessageBody;

        let registry = SubscriptionRegistry::new();
        let (sub, mut rx) = registry.create(1, None);
        (find_stream_handler("CodeStudioSessionStreamRequest")
            .unwrap()
            .handler_fn)(MessageBody::ModelListRequest, code_studio_test_ctx(), sub);
        assert!(matches!(
            rx.recv().await,
            Some(SubscriptionEvent::End(None))
        ));
    }

    /// The attribute word is the only place a cell's colour survives: bits 0..7
    /// are the style flags, 8..15 the foreground, 16..23 the background, and
    /// bits 24/25 say whether each colour was set at all. A cell with no colour
    /// must not claim index 0 — that is black, not "the theme's default".
    #[test]
    fn a_terminal_attribute_word_packs_colour_and_style() {
        use crate::code_studio::terminal::{attrs, Cell, Color};

        assert_eq!(super::code_studio_pack_attrs(&Cell::default()), 0);

        let styled = Cell {
            ch: 'x',
            fg: Color::Indexed(9),
            bg: Color::Indexed(2),
            attrs: attrs::BOLD | attrs::UNDERLINE,
        };
        let word = super::code_studio_pack_attrs(&styled);
        assert_eq!(word & 0xff, u32::from(attrs::BOLD | attrs::UNDERLINE));
        assert_eq!((word >> 8) & 0xff, 9);
        assert_eq!((word >> 16) & 0xff, 2);
        assert_eq!(word >> 24, 0b11);

        // True colour is quantised: pure red lands on the 6×6×6 cube, a neutral
        // grey on the 24-step ramp the cube's six levels would band.
        assert_eq!(super::code_studio_quantize_rgb(255, 0, 0), 196);
        assert_eq!(super::code_studio_quantize_rgb(128, 128, 128), 244);
    }

    /// The trailing half of a double-width character is a placeholder in the
    /// grid, not a character: emitting it would put one attribute word too many
    /// next to the text and shift every colour after it.
    #[test]
    fn a_double_width_placeholder_never_reaches_the_wire() {
        use crate::code_studio::terminal::{Cell, GridRow};

        let cells = vec![
            Cell {
                ch: '字',
                ..Cell::default()
            },
            Cell {
                ch: '\0',
                ..Cell::default()
            },
            Cell {
                ch: 'a',
                ..Cell::default()
            },
        ];
        let rows = super::code_studio_cell_rows(&[GridRow {
            index: 3,
            revision: 7,
            cells,
        }]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row, 3);
        assert_eq!(rows[0].text, "字a");
        assert_eq!(rows[0].attrs.len(), 2);
    }

    /// §12.2: output above the frame budget goes to the artifact, not into the
    /// frame. The body is already in the CAS, redacted, so the frame names it
    /// instead of carrying a truncated copy that would look like the whole
    /// thing.
    #[test]
    fn an_oversized_event_body_travels_as_its_artifact_reference() {
        use crate::code_studio::events::{EventKind, EventPayload, StoredEvent};

        let small = StoredEvent {
            event_id: "e1".into(),
            seq: 1,
            kind: EventKind::AgentMessage,
            run_id: None,
            agent_id: None,
            payload: EventPayload::AgentMessage {
                role: "assistant".into(),
                text: "ok".into(),
            },
            artifact_ref: None,
            security_relevant: false,
            created_at: "2026-08-14T10:00:00Z".into(),
        };
        assert!(super::code_studio_event_payload_json(&small).contains("\"ok\""));

        let huge = StoredEvent {
            payload: EventPayload::AgentMessage {
                role: "assistant".into(),
                text: "x".repeat(super::CS_MAX_EVENT_PAYLOAD_BYTES + 1),
            },
            artifact_ref: Some("sha256:abc".into()),
            ..small
        };
        let rendered = super::code_studio_event_payload_json(&huge);
        assert!(rendered.len() < 512, "{rendered}");
        assert!(rendered.contains("\"oversized\":true"), "{rendered}");
        assert!(rendered.contains("sha256:abc"), "{rendered}");
        assert!(!rendered.contains("xxxx"), "{rendered}");
    }

    // ---- Code Studio streams across the mesh (§12.2) ----

    const CS_TEST_WORKSPACE: &str = "ws-1";
    const CS_TEST_SESSION: &str = "sess-1";
    /// Consumer of the hub streams below. A stream is keyed by the PERSON as
    /// well as the node, so a test that pulls has to name one.
    const CS_TEST_USER: &str = "user-1";

    /// Seeds an org, a user, a workspace owned by `owner_node` and a workspace
    /// membership, then returns the request context the handlers see. The gate
    /// re-reads all of it from the database, so the rows — not the context —
    /// are what decides.
    fn code_studio_seeded_ctx(owner_node: &str) -> super::super::HandlerContext {
        use crate::services::org::repo as org_repo;
        use rusqlite::params;

        let state = super::super::state::AppState::for_test();
        let user_id = uuid::Uuid::new_v4().to_string();
        let org = org_repo::create_organization(&state.db, "Acme", "acme", None, None, None, None)
            .expect("create org");
        let role_id = org_repo::list_roles(&state.db)
            .expect("roles")
            .into_iter()
            .find(|role| role.name == "org_admin")
            .expect("org_admin must be seeded by the migrations")
            .role_id;
        org_repo::add_membership(&state.db, &org.org_id, &user_id, &role_id, &user_id)
            .expect("org membership");
        {
            let conn = state.db.write().expect("db");
            conn.execute(
                "INSERT OR IGNORE INTO user_accounts \
                   (id, username, password_hash, display_name, email, is_active, is_admin, \
                    created_at, updated_at, role) \
                 VALUES (?1, ?1, 'x', ?1, ?1, 1, 0, datetime('now'), datetime('now'), 'user')",
                params![user_id],
            )
            .expect("seed user");
            conn.execute(
                "INSERT OR IGNORE INTO code_workspaces \
                   (id, org_id, owner_user_id, name, slug, node_id, exec_mode, \
                    egress_enforcement, repo_kind, autonomy_ceiling, egress_policy, \
                    index_enabled, status, created_at, updated_at) \
                 VALUES (?4, ?2, ?1, 'W', 'w', ?3, 'trusted_native', \
                    'unrestricted', 'empty', 'normal', 'org_approved', 0, 'active', \
                    datetime('now'), datetime('now'))",
                params![user_id, org.org_id, owner_node, CS_TEST_WORKSPACE],
            )
            .expect("seed workspace");
            conn.execute(
                "INSERT OR REPLACE INTO code_workspace_members \
                   (workspace_id, user_id, role, added_by, added_at) \
                 VALUES (?2, ?1, 'owner', ?1, datetime('now'))",
                params![user_id, CS_TEST_WORKSPACE],
            )
            .expect("seed membership");
        }
        let org_context =
            crate::services::rbac::resolve_org_context(&state.db, &user_id, Some(&org.org_id))
                .expect("org context");

        super::super::HandlerContext {
            session: tentaflow_protocol::SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: None,
            },
            correlation_id: 7,
            connection_id: 0,
            resume_secret: None,
            state,
            org_context: Some(org_context),
        }
    }

    /// §12.2 landed: a workspace owned by another node is no longer a dead end.
    /// Both live streams take the mesh pull path, and a node this one cannot
    /// reach — here because the test process runs no mesh at all — ends the
    /// subscription with a CONNECTIVITY reason. Never `workspace_not_local`,
    /// which would claim the feature does not exist, and never a session
    /// verdict, which would claim somebody's unfinished work had ended.
    #[tokio::test]
    async fn a_remote_workspace_stream_takes_the_pull_path_instead_of_refusing() {
        use super::super::subscription::SubscriptionRegistry;
        use tentaflow_protocol::code_studio::CodeStudioPayload;
        use tentaflow_protocol::MessageBody;

        let registry = SubscriptionRegistry::new();

        let (sub, mut rx) = registry.create(1, None);
        (find_stream_handler("CodeStudioSessionStreamRequest")
            .unwrap()
            .handler_fn)(
            MessageBody::CodeStudioBody(CodeStudioPayload::SessionStreamRequest {
                workspace_id: CS_TEST_WORKSPACE.into(),
                session_id: CS_TEST_SESSION.into(),
                after_seq: 0,
            }),
            code_studio_seeded_ctx("node-b"),
            sub,
        );
        match rx.recv().await {
            Some(SubscriptionEvent::End(Some(MessageBody::CodeStudioBody(
                CodeStudioPayload::SessionStreamEnd { reason },
            )))) => assert_eq!(reason, "owner_unreachable"),
            other => panic!("expected the timeline to take the pull path, got {other:?}"),
        }

        let (sub, mut rx) = registry.create(2, None);
        (find_stream_handler("CodeStudioTerminalStreamRequest")
            .unwrap()
            .handler_fn)(
            MessageBody::CodeStudioBody(CodeStudioPayload::TerminalStreamRequest {
                workspace_id: CS_TEST_WORKSPACE.into(),
                session_id: CS_TEST_SESSION.into(),
                terminal_id: "t1".into(),
                after_revision: 0,
            }),
            code_studio_seeded_ctx("node-b"),
            sub,
        );
        match rx.recv().await {
            Some(SubscriptionEvent::End(Some(MessageBody::CodeStudioBody(
                CodeStudioPayload::TerminalStreamEnd { reason },
            )))) => assert_eq!(reason, "owner_unreachable"),
            other => panic!("expected the terminal to take the pull path, got {other:?}"),
        }
    }

    /// The index runs where the repository is, so a remote workspace's progress
    /// is pulled like every other stream — and an owner node this process
    /// cannot reach is CONNECTIVITY, never a claim that the feature is missing.
    #[tokio::test]
    async fn a_remote_index_stream_takes_the_pull_path() {
        use super::super::subscription::SubscriptionRegistry;
        use tentaflow_protocol::code_studio::CodeStudioPayload;
        use tentaflow_protocol::MessageBody;

        let registry = SubscriptionRegistry::new();
        let (sub, mut rx) = registry.create(1, None);
        (find_stream_handler("CodeStudioIndexStreamRequest")
            .unwrap()
            .handler_fn)(
            MessageBody::CodeStudioBody(CodeStudioPayload::IndexStreamRequest {
                workspace_id: CS_TEST_WORKSPACE.into(),
                after_seq: 0,
            }),
            code_studio_seeded_ctx("node-b"),
            sub,
        );
        match rx.recv().await {
            Some(SubscriptionEvent::End(Some(MessageBody::CodeStudioBody(
                CodeStudioPayload::IndexStreamEnd { reason },
            )))) => assert_eq!(reason, "owner_unreachable"),
            other => panic!("expected the index to take the pull path, got {other:?}"),
        }
    }

    /// A workspace whose index is switched off says so rather than holding a
    /// subscription open that nothing will ever write to.
    #[tokio::test]
    async fn an_index_stream_on_a_workspace_without_an_index_says_so() {
        use super::super::subscription::SubscriptionRegistry;
        use tentaflow_protocol::code_studio::CodeStudioPayload;
        use tentaflow_protocol::MessageBody;

        // The fixture seeds `index_enabled = 0`, and the local node owns it.
        let ctx = code_studio_seeded_ctx("test-node");
        let registry = SubscriptionRegistry::new();
        let (sub, mut rx) = registry.create(1, None);
        (find_stream_handler("CodeStudioIndexStreamRequest")
            .unwrap()
            .handler_fn)(
            MessageBody::CodeStudioBody(CodeStudioPayload::IndexStreamRequest {
                workspace_id: CS_TEST_WORKSPACE.into(),
                after_seq: 0,
            }),
            ctx,
            sub,
        );
        match rx.recv().await {
            Some(SubscriptionEvent::End(Some(MessageBody::CodeStudioBody(
                CodeStudioPayload::IndexStreamEnd { reason },
            )))) => assert_eq!(reason, "index_unavailable"),
            other => panic!("expected index_unavailable, got {other:?}"),
        }
    }

    fn cs_event(seq: u64) -> tentaflow_protocol::code_studio::CodeStudioPayload {
        tentaflow_protocol::code_studio::CodeStudioPayload::SessionStreamEvent {
            seq,
            kind: "agent_message".into(),
            run_id: None,
            agent_id: None,
            created_at: "2026-08-14T10:00:00Z".into(),
            payload_json: "{\"text\":\"ok\"}".into(),
            security_relevant: false,
        }
    }

    /// The two halves of §12.2 have to agree byte for byte. This drives the
    /// REAL hub — what an owner node publishes — through the REAL cursor and
    /// into the decoder the pull loop uses, so it covers the frame encoding,
    /// the cursor advance, the client's own resume point and the close reason
    /// in one pass. What it does NOT cover is the mesh round trip between them.
    #[tokio::test]
    async fn frames_published_on_the_owner_hub_decode_into_the_frames_the_client_gets() {
        use crate::code_studio::mesh_stream::{
            StreamCursor, StreamHub, StreamOpen, KIND_DATA, REASON_SESSION_CLOSED,
        };
        use tentaflow_protocol::code_studio::CodeStudioPayload;

        let hub = StreamHub::default();
        let handle = hub.open(StreamOpen {
            session_id: CS_TEST_SESSION.into(),
            stream_id: super::CS_STREAM_TIMELINE.into(),
            workspace_id: CS_TEST_WORKSPACE.into(),
            consumer_node_id: "node-a".into(),
            consumer_user_id: CS_TEST_USER.into(),
            window: 0,
            inline_budget: 0,
            snapshot: None,
        });
        for seq in 1..=3u64 {
            handle
                .publish(
                    KIND_DATA,
                    0,
                    crate::mesh::cbor::encode(&cs_event(seq)).expect("encode"),
                )
                .await
                .expect("publish");
        }

        let mut cursor = StreamCursor::default();
        let batch = cursor.accept(
            hub.pull_for_peer(
                "node-a",
                CS_TEST_USER,
                CS_TEST_SESSION,
                super::CS_STREAM_TIMELINE,
                cursor.after_seq(),
                cursor.acked_seq,
                64,
            )
            .expect("pull"),
        );
        assert_eq!(cursor.last_seq, 3, "the cursor advances over the batch");

        // The browser resumed at event 1, so it must not be handed it again.
        let (frames, closed) =
            super::code_studio_decode_batch(super::CodeStudioRemoteStream::Timeline, batch, 1);
        assert!(closed.is_none(), "a live stream does not end");
        let seqs: Vec<u64> = frames
            .iter()
            .map(|frame| match frame {
                CodeStudioPayload::SessionStreamEvent { seq, .. } => *seq,
                other => panic!("the timeline decoded into {other:?}"),
            })
            .collect();
        assert_eq!(seqs, vec![2, 3]);
        assert_eq!(frames[0], cs_event(2), "the frame survives the round trip");

        // The owner ends the session; the reason travels verbatim.
        hub.close_session(CS_TEST_SESSION, REASON_SESSION_CLOSED, "operator closed it");
        let batch = cursor.accept(
            hub.pull_for_peer(
                "node-a",
                CS_TEST_USER,
                CS_TEST_SESSION,
                super::CS_STREAM_TIMELINE,
                cursor.after_seq(),
                cursor.acked_seq,
                64,
            )
            .expect("pull"),
        );
        let (frames, closed) =
            super::code_studio_decode_batch(super::CodeStudioRemoteStream::Timeline, batch, 3);
        assert!(frames.is_empty());
        assert_eq!(
            closed.as_deref(),
            Some("session_closed"),
            "the owner's reason is the reason"
        );
    }

    /// A terminal grid crosses the same seam, and the client's `after_revision`
    /// is applied to the DECODED frame: a snapshot at the revision it already
    /// holds is the screen it is already showing.
    #[test]
    fn a_terminal_grid_crosses_the_seam_and_honours_the_clients_revision() {
        use crate::code_studio::mesh_stream::{ConsumedBatch, KIND_DATA};
        use tentaflow_protocol::code_studio::{
            CodeStudioPayload, TerminalCellRow, TerminalCursorInfo,
        };
        use tentaflow_protocol::mesh::CodeStudioStreamFrame;

        let snapshot = CodeStudioPayload::TerminalStreamSnapshot {
            revision: 12,
            grid_rows: 24,
            grid_cols: 80,
            cursor: TerminalCursorInfo {
                row: 1,
                col: 2,
                visible: true,
            },
            rows: vec![TerminalCellRow {
                row: 0,
                text: "$ ls".into(),
                attrs: vec![0, 0, 0, 0],
            }],
        };
        let delta = CodeStudioPayload::TerminalStreamDelta {
            revision: 13,
            grid_rows: 24,
            grid_cols: 80,
            cursor: TerminalCursorInfo {
                row: 2,
                col: 0,
                visible: true,
            },
            rows: Vec::new(),
        };
        let wire = |payload: &CodeStudioPayload| CodeStudioStreamFrame {
            session_id: CS_TEST_SESSION.into(),
            stream_id: super::code_studio_terminal_stream_id("t1"),
            seq: 1,
            kind: KIND_DATA.into(),
            revision: 0,
            data: crate::mesh::cbor::encode(payload).expect("encode"),
        };

        let (frames, closed) = super::code_studio_decode_batch(
            super::CodeStudioRemoteStream::Terminal,
            ConsumedBatch {
                frames: vec![wire(&snapshot), wire(&delta)],
                duplicates: 0,
                close: None,
            },
            12,
        );
        assert!(closed.is_none());
        assert_eq!(
            frames,
            vec![delta],
            "the snapshot the client already holds is dropped, the delta is not"
        );
    }

    /// The owner node authorizes; it does not get to decide what the browser
    /// renders. A frame of the wrong variant, and output that overflowed into
    /// an artifact the browser cannot fetch, both END the stream with a named
    /// reason instead of being forwarded or silently dropped.
    #[test]
    fn a_frame_that_does_not_belong_to_the_stream_ends_it() {
        use crate::code_studio::mesh_stream::{ConsumedBatch, KIND_ARTIFACT, KIND_DATA};
        use tentaflow_protocol::mesh::CodeStudioStreamFrame;

        let frame = |kind: &str, data: Vec<u8>| CodeStudioStreamFrame {
            session_id: CS_TEST_SESSION.into(),
            stream_id: super::CS_STREAM_TIMELINE.into(),
            seq: 1,
            kind: kind.into(),
            revision: 0,
            data,
        };

        // A terminal frame on the timeline is build drift, not content.
        let (frames, closed) = super::code_studio_decode_batch(
            super::CodeStudioRemoteStream::Timeline,
            ConsumedBatch {
                frames: vec![frame(
                    KIND_DATA,
                    crate::mesh::cbor::encode(
                        &tentaflow_protocol::code_studio::CodeStudioPayload::TerminalStreamEnd {
                            reason: "whatever".into(),
                        },
                    )
                    .expect("encode"),
                )],
                duplicates: 0,
                close: None,
            },
            0,
        );
        assert!(frames.is_empty());
        assert_eq!(closed.as_deref(), Some("internal_error"));

        let (frames, closed) = super::code_studio_decode_batch(
            super::CodeStudioRemoteStream::Timeline,
            ConsumedBatch {
                frames: vec![
                    frame(
                        KIND_DATA,
                        crate::mesh::cbor::encode(&cs_event(9)).expect("encode"),
                    ),
                    frame(KIND_ARTIFACT, b"sha256:abc".to_vec()),
                ],
                duplicates: 0,
                close: None,
            },
            0,
        );
        assert_eq!(
            frames,
            vec![cs_event(9)],
            "what was already decoded is still delivered"
        );
        assert_eq!(closed.as_deref(), Some("stream_overflow"));
    }

    /// The stream id is half of the contract with the owner node, and the owner
    /// node treats it as untrusted input: a timeline and a terminal must name a
    /// session, a terminal id goes through the filesystem alphabet guard, and
    /// an index stream must name the workspace it was authorized for — without
    /// that last rule two workspaces watched from one node would share a key.
    #[test]
    fn an_owner_stream_id_is_validated_against_the_rest_of_the_request() {
        use tentaflow_protocol::mesh::CodeStudioStreamOpenRequest;

        let request = |session: &str, stream: &str| CodeStudioStreamOpenRequest {
            workspace_id: "ws-1".into(),
            session_id: session.into(),
            stream_id: stream.into(),
            after_revision: 0,
            window: 0,
        };
        let parsed = |session: &str, stream: &str| {
            super::code_studio_owner_stream(&request(session, stream)).is_some()
        };

        assert!(parsed("sess-1", super::CS_STREAM_TIMELINE));
        assert!(parsed("sess-1", "terminal:t1"));
        assert!(parsed("", "index:ws-1"));

        assert!(
            !parsed("", super::CS_STREAM_TIMELINE),
            "a timeline needs a session"
        );
        assert!(!parsed("", "terminal:t1"), "a terminal needs a session");
        assert!(
            !parsed("sess-1", "index:ws-1"),
            "the index is workspace scoped"
        );
        assert!(
            !parsed("", "index:ws-2"),
            "an index stream may only name the workspace it was authorized for"
        );
        assert!(
            !parsed("sess-1", "terminal:../etc"),
            "the id alphabet holds"
        );
        assert!(
            !parsed("sess-1", "terminal:"),
            "an empty terminal id is not an id"
        );
        assert!(!parsed("sess-1", "something-else"));
    }

    /// Each shell is its own stream, so two terminals of one session cannot
    /// collide in the hub — the ids are half of the contract with the owner
    /// node and the only thing keeping them apart.
    #[test]
    fn every_terminal_of_a_session_is_its_own_stream() {
        assert_ne!(
            super::code_studio_terminal_stream_id("t1"),
            super::code_studio_terminal_stream_id("t2")
        );
        assert_ne!(
            super::code_studio_terminal_stream_id("t1"),
            super::CS_STREAM_TIMELINE
        );
    }

    /// Sessions are private to the person who opened them (§5.3), with no
    /// administrator override (§25.4). This is the ONE gate both paths run —
    /// the browser attached to this node and, through
    /// `remote_proxy::open_owner_stream`, every stream command arriving over
    /// the mesh — so the property is proved once for both.
    mod code_studio_stream_gate {
        use rusqlite::params;

        const ORG: &str = "org-cs-streams";
        const NODE: &str = "node-owner";

        struct Fixture {
            _data: tempfile::TempDir,
            _registry: tempfile::TempDir,
            _guard: std::sync::MutexGuard<'static, ()>,
            db: crate::db::DbPool,
            /// Unique per fixture: `workspace_db` caches pools by id for the
            /// whole process, so two tests sharing an id would share a database
            /// whose directory the first one already deleted.
            workspace: String,
            alice: String,
            bob: String,
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
            }
        }

        fn seed_user(db: &crate::db::DbPool, workspace: &str, role_id: &str) -> String {
            let user_id = uuid::Uuid::new_v4().to_string();
            let conn = db.write().expect("db");
            conn.execute(
                "INSERT INTO user_accounts \
                   (id, username, password_hash, display_name, email, is_active, is_admin, \
                    created_at, updated_at, role) \
                 VALUES (?1, ?1, 'x', ?1, ?1, 1, 0, datetime('now'), datetime('now'), 'user')",
                params![user_id],
            )
            .expect("account");
            conn.execute(
                "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
                 VALUES (?1, ?2, ?3, datetime('now'), ?2)",
                params![ORG, user_id, role_id],
            )
            .expect("org membership");
            conn.execute(
                "INSERT INTO code_workspace_members \
                   (workspace_id, user_id, role, added_by, added_at) \
                 VALUES (?1, ?2, 'editor', ?2, datetime('now'))",
                params![workspace, user_id],
            )
            .expect("workspace membership");
            user_id
        }

        fn fixture() -> Fixture {
            let guard = crate::code_studio::paths::test_data_dir_guard();
            let data = tempfile::tempdir().expect("data dir");
            crate::paths::set_category_override(
                crate::paths::StorageCategory::Data,
                Some(data.path().to_string_lossy().to_string()),
            );
            let registry = tempfile::tempdir().expect("registry dir");
            let db = crate::db::init(&registry.path().join("tentaflow.db")).expect("init db");
            let workspace = format!("ws-{}", uuid::Uuid::new_v4());
            let role_id = crate::services::org::repo::list_roles(&db)
                .expect("roles")
                .into_iter()
                .find(|r| r.name == "org_admin")
                .expect("org_admin is seeded by the migrations")
                .role_id;
            {
                let conn = db.write().expect("db");
                conn.execute(
                    "INSERT INTO organizations (org_id, name, slug, status, created_at) \
                     VALUES (?1, 'Streams', ?1, 'active', datetime('now'))",
                    params![ORG],
                )
                .expect("org");
                conn.execute(
                    "INSERT INTO code_workspaces \
                       (id, org_id, owner_user_id, name, slug, node_id, exec_mode, \
                        egress_enforcement, repo_kind, autonomy_ceiling, egress_policy, \
                        index_enabled, status, created_at, updated_at) \
                     VALUES (?1, ?2, 'seed', 'W', 'w', ?3, 'trusted_native', 'unrestricted', \
                        'empty', 'normal', 'org_approved', 0, 'active', datetime('now'), \
                        datetime('now'))",
                    params![workspace, ORG, NODE],
                )
                .expect("workspace");
            }
            let alice = seed_user(&db, &workspace, &role_id);
            let bob = seed_user(&db, &workspace, &role_id);

            let dir = crate::code_studio::paths::workspace_dir(&workspace).expect("workspace dir");
            std::fs::create_dir_all(&dir).expect("workspace directory");
            let pool = crate::code_studio::workspace_db::open(&workspace).expect("workspace db");
            {
                let conn = pool.write().expect("workspace db");
                for (session, user) in [("sess-alice", &alice), ("sess-bob", &bob)] {
                    conn.execute(
                        "INSERT INTO sessions (id, workspace_id, user_id, title, branch, \
                          autonomy_mode, flow_id, flow_version_id, status, created_at, updated_at) \
                         VALUES (?1, ?2, ?3, 'Session', 'cs/' || ?1, 'normal', 'flow', 'v1', \
                          'running', datetime('now'), datetime('now'))",
                        params![session, workspace, user],
                    )
                    .expect("session");
                }
            }

            Fixture {
                _data: data,
                _registry: registry,
                _guard: guard,
                db,
                workspace,
                alice,
                bob,
            }
        }

        fn authorize(f: &Fixture, user: &str, session: &str) -> Result<(), &'static str> {
            super::super::code_studio_authorize_stream(
                &f.db,
                NODE,
                user,
                ORG,
                &f.workspace,
                session,
            )
            .map(|_| ())
        }

        /// The hole this closes: a stream used to be bound to a NODE, so a
        /// member of the workspace could read a colleague's session by naming
        /// its id. The refusal is also WORD FOR WORD the one a session that
        /// does not exist produces, so it cannot be used to discover that one
        /// does.
        #[test]
        fn another_persons_session_is_refused_exactly_like_a_missing_one() {
            let f = fixture();
            authorize(&f, &f.alice, "sess-alice").expect("her own session");
            authorize(&f, &f.bob, "sess-bob").expect("his own session");

            let theirs = authorize(&f, &f.bob, "sess-alice").expect_err("not his session");
            let missing = authorize(&f, &f.bob, "sess-nobody").expect_err("no such session");
            assert_eq!(theirs, super::super::CS_END_NOT_FOUND);
            assert_eq!(theirs, missing, "the two refusals must be identical");

            // And the wire form is one message with no id in it, so the two are
            // indistinguishable on the other node as well.
            assert_eq!(
                crate::code_studio::remote_proxy::STREAM_NOT_FOUND,
                "code studio session not found"
            );
        }

        /// `bob` is an `org_admin` holding `code_studio.admin` and a member of
        /// the workspace. §25.4 gives the administrator metadata and lifecycle,
        /// never the content of somebody's session.
        #[test]
        fn an_administrator_gets_the_same_answer_as_a_stranger() {
            let f = fixture();
            let org = crate::services::rbac::resolve_org_context(&f.db, &f.bob, Some(ORG))
                .expect("org context");
            assert!(
                org.has("code_studio.admin"),
                "the fixture must give bob the administrator permission"
            );
            assert_eq!(
                authorize(&f, &f.bob, "sess-alice").expect_err("still not his session"),
                super::super::CS_END_NOT_FOUND
            );
        }

        /// Re-authorization reads the DATABASE, not a snapshot taken when the
        /// stream opened: this is the call a producer repeats every
        /// `CS_REVALIDATE_EVERY`, and every mesh pull repeats its workspace
        /// half. A role taken away therefore ends the stream with a named
        /// reason.
        #[test]
        fn a_revoked_role_is_seen_by_the_next_re_authorization() {
            let f = fixture();
            authorize(&f, &f.alice, "sess-alice").expect("permitted before");

            {
                let conn = f.db.write().expect("db");
                conn.execute(
                    "DELETE FROM code_workspace_members WHERE workspace_id = ?1 AND user_id = ?2",
                    params![f.workspace, f.alice],
                )
                .expect("revoke membership");
            }
            assert_eq!(
                authorize(&f, &f.alice, "sess-alice").expect_err("membership is gone"),
                super::super::CS_END_NOT_FOUND
            );

            // Losing the org role names the loss for what it is, and that word
            // is the one the stream closes with.
            {
                let conn = f.db.write().expect("db");
                conn.execute(
                    "DELETE FROM org_memberships WHERE org_id = ?1 AND user_id = ?2",
                    params![ORG, f.bob],
                )
                .expect("revoke org role");
            }
            assert_eq!(
                authorize(&f, &f.bob, "sess-bob").expect_err("org role is gone"),
                super::super::CS_END_PERMISSION_REVOKED
            );
        }

        /// A revoked reader is not answered with an empty batch: the stream is
        /// closed with the gate's own reason and the next pull carries it. This
        /// is exactly the pair of steps `remote_proxy::pull_owner_stream` takes.
        #[tokio::test]
        async fn a_revoked_reader_gets_the_close_record_not_silence() {
            let f = fixture();
            let hub = crate::code_studio::mesh_stream::StreamHub::default();
            let handle = hub.open(crate::code_studio::mesh_stream::StreamOpen {
                session_id: "sess-alice".into(),
                stream_id: super::super::CS_STREAM_TIMELINE.into(),
                workspace_id: f.workspace.clone(),
                consumer_node_id: "node-dashboard".into(),
                consumer_user_id: f.alice.clone(),
                window: 8,
                inline_budget: 0,
                snapshot: None,
            });
            handle
                .publish(crate::code_studio::mesh_stream::KIND_DATA, 1, vec![1])
                .await
                .expect("publish");

            {
                let conn = f.db.write().expect("db");
                conn.execute(
                    "DELETE FROM org_memberships WHERE org_id = ?1 AND user_id = ?2",
                    params![ORG, f.alice],
                )
                .expect("revoke org role");
            }
            let reason = authorize(&f, &f.alice, "sess-alice").expect_err("access ended");
            hub.close_session("sess-alice", reason, "re-checked against the database");

            let result = hub
                .pull_for_peer(
                    "node-dashboard",
                    &f.alice,
                    "sess-alice",
                    super::super::CS_STREAM_TIMELINE,
                    0,
                    0,
                    64,
                )
                .expect("the close record is still readable");
            assert_eq!(
                result.close.expect("closed").reason,
                super::super::CS_END_PERMISSION_REVOKED
            );
        }
    }
}
