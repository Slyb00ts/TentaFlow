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
        let route_result = match router
            .route_chat_completion_stream(request, user, None, flow_selector)
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
    use crate::flow_engine::dispatcher::FlowRequestMeta;
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
    let correlation_id = ctx.correlation_id;
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
        let envelope =
            match flow_envelope_from_inputs(invoke.inputs, resolved_language, &blobs).await {
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
        let mut meta = FlowRequestMeta::new(format!("flowinvoke-{correlation_id}"));
        meta.session_id = invoke.session_id.clone();
        meta.user_id = actor_id.clone();
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
// ProjectStudioChatStream — project chat turn over the system ps-chat flow.
// =============================================================================
// The frontend subscribes via ApiBinary.subscribe('projectStudioChatStreamRequest',
// { projectId, chatId, message }). One turn: persist the user message, run the
// seeded ps-chat flow (trigger -> project_knowledge -> conversation_history ->
// llm streaming), forward tokens as ChatStreamChunk{kind:"token"}, emit ONE
// ChatStreamChunk{kind:"citations"} from the final envelope's rag_citations,
// persist the assistant reply (content + citations_json) and finish with
// ChatStreamEnd{message_id}. Dropping the subscription cancels the generation
// (push failure fires the flow's cancel token — same contract as FlowInvoke).

/// Chat model for a project turn: the project's 'chat' agent binding
/// (`settings['agents']` in project.db) wins; without a binding (or an agent
/// without a model) returns None and the llm node's hard "no model" error
/// surfaces through ChatStreamEnd — the platform deliberately has no global
/// default model (see `build_initial_envelope_inner`).
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
    use crate::flow_engine::dispatcher::FlowRequestMeta;
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
    let correlation_id = ctx.correlation_id;

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
        if let Some(m) = model {
            envelope
                .meta
                .insert("model".into(), serde_json::Value::String(m));
        }

        // Cancel bound to the subscription: a dropped/unsubscribed client
        // aborts the flow (push failure below fires this token).
        let cancel = CancellationToken::new();
        let mut meta = FlowRequestMeta::new(format!("ps-chat-{correlation_id}"));
        meta.session_id = Some(chat.session_id.clone());
        meta.user_id = Some(caller_id.clone());
        meta.user_role = user_role;
        meta.org_id = Some(org_id);
        meta.cancel_token = cancel.clone();

        let dispatch = fd
            .dispatch_by_flow_id_streaming(
                crate::db::seed::PS_CHAT_FLOW_ID.to_string(),
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
                // ps-chat is text-only; other delta kinds carry no tokens.
                Ok(_) => {}
                Err(e) => {
                    cancel.cancel();
                    end_error(&sub, &chat_id, format!("stream error: {e}"));
                    return;
                }
            }
        }

        // The final envelope carries the retrieval citations set by the
        // project_knowledge node (meta["rag_citations"]).
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
// tools would execute them. A dedicated system flow (the `ps-chat` pattern)
// would close that off; it needs its own seed + streaming contract, so it is
// deliberately NOT bolted on here.

fn project_studio_code_assist_stream_handler(
    req: MessageBody,
    ctx: HandlerContext,
    sub: Arc<Subscription>,
) {
    use crate::flow_engine::dispatcher::FlowRequestMeta;
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
    let correlation_id = ctx.correlation_id;

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
        let mut meta = FlowRequestMeta::new(format!("ps-assist-{correlation_id}"));
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
}
