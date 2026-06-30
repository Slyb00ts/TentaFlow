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
            memory_options: None,
            audio_input: None,
        };

        // Selektor flow z UI czatu: konkretny flow po ID albo synthetic
        // "Default Chat". Celowo NIE Auto — Auto rozwiązuje default flow z DB
        // (edytowalny w Flow Builderze), a UI czatu ma jawny wybór flow.
        let flow_selector = match stream_req.flow_id.clone() {
            Some(flow_id) => crate::routing::streaming::ChatFlowSelector::FlowId(flow_id),
            None => crate::routing::streaming::ChatFlowSelector::Synthetic,
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
        SessionAuth::UserSession { user_id, .. } => Some(
            uuid::Uuid::from_bytes(*user_id)
                .hyphenated()
                .to_string(),
        ),
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
                    let Some((bw, lat_us, rdma)) = run_interface_probe(
                        &qm,
                        &local_id,
                        x,
                        y,
                        &nonce,
                        NUM_STREAMS,
                        DURATION_MS,
                    )
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
                    if nif.rdma_available { "rdma" } else { "ethernet" }.to_string(),
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
            }],
            temperature: None,
            max_tokens: None,
            flow_id: None,
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
