// =============================================================================
// Plik: roles/reverse_proxy.rs
// Opis: Rola ReverseProxy — sidecar nasluchuje iroh od routera i forwarduje
//       requesty do lokalnego HTTP API silnika (vLLM, llama.cpp-server, sglang,
//       sherpa itp). Obsluguje OpenAI-compatible chat/embeddings + raw HTTP
//       passthrough. SSE z upstreamu mapowane na ModelStreamChunk.
// =============================================================================

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use tentaflow_protocol::{
    CompletionPayload, CompletionResult, EmbeddingsPayload, EmbeddingsResult, ErrorInfo,
    ErrorType, ModelPayload, ModelRequest, ModelResponse, ModelResult, ModelStreamChunk,
    StreamChunkType,
};
use tentaflow_transport::{
    build_server_endpoint, serve_model_requests, HandleError, ModelHandler, ModelOutcome,
    ServerEndpointConfig, ALPN_SERVICE,
};
use tokio::sync::watch;

use crate::config::{Role, SidecarConfig, UpstreamApi};
use crate::identity;

pub async fn run(config: SidecarConfig) -> Result<()> {
    let (upstream_url, timeout_ms, api) = match &config.role {
        Role::ReverseProxy {
            upstream_url,
            timeout_ms,
            api,
        } => (upstream_url.clone(), *timeout_ms, api.clone()),
        _ => anyhow::bail!("ReverseProxy::run wywolany z bledna rola"),
    };

    let handler = Arc::new(ReverseProxyHandler::new(
        upstream_url,
        timeout_ms,
        api,
        config.model_aliases.clone(),
    )?);

    let secret_key = identity::load_or_generate(config.transport.secret_key_path.as_deref())?;
    let bind_addr: SocketAddr = format!("0.0.0.0:{}", config.transport.port).parse()?;

    let endpoint = build_server_endpoint(ServerEndpointConfig {
        secret_key,
        bind_addr,
        alpns: vec![ALPN_SERVICE.to_vec()],
        relay_url: None,
        enable_lan_discovery: config.transport.enable_lan_discovery,
        enable_dht_discovery: config.transport.enable_dht_discovery,
    })
    .await?;

    tracing::info!(
        endpoint_id = %endpoint.id().fmt_short(),
        bind = %bind_addr,
        "Sidecar iroh endpoint gotowy"
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let sh_tx = shutdown_tx.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("Ctrl+C — wysylam shutdown");
        let _ = sh_tx.send(true);
    });

    serve_model_requests(endpoint, handler, shutdown_rx).await?;
    tracing::info!("ReverseProxy: zakonczony");
    Ok(())
}

/// Handler ktory tlumaczy ModelRequest ↔ lokalne HTTP API silnika.
struct ReverseProxyHandler {
    client: reqwest::Client,
    upstream: String,
    api: UpstreamApi,
    #[allow(dead_code)]
    aliases: Vec<String>,
}

impl ReverseProxyHandler {
    fn new(
        upstream: String,
        timeout_ms: u64,
        api: UpstreamApi,
        aliases: Vec<String>,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()?;
        Ok(Self {
            client,
            upstream,
            api,
            aliases,
        })
    }

    async fn handle_chat(
        &self,
        request: &ModelRequest,
        payload: &CompletionPayload,
    ) -> Result<ModelOutcome, HandleError> {
        let url = match self.api {
            UpstreamApi::OpenAi => format!("{}/chat/completions", self.upstream.trim_end_matches('/')),
            UpstreamApi::LlamaCpp => format!("{}/v1/chat/completions", self.upstream.trim_end_matches('/')),
            UpstreamApi::Sherpa => {
                return Err(HandleError::UnsupportedRequest(
                    "Sherpa API nie obsluguje chat".into(),
                ))
            }
            UpstreamApi::RawHttp => format!("{}/chat/completions", self.upstream.trim_end_matches('/')),
        };

        let mut body = serde_json::json!({
            "model": payload.model,
            "stream": request.stream,
        });
        if !payload.messages.is_empty() {
            let msgs: Vec<_> = payload
                .messages
                .iter()
                .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
                .collect();
            body["messages"] = serde_json::Value::Array(msgs);
        } else if let Some(prompt) = &payload.prompt {
            body["messages"] = serde_json::json!([{ "role": "user", "content": prompt }]);
        }
        if let Some(t) = payload.temperature {
            body["temperature"] = t.into();
        }
        if let Some(mt) = payload.max_tokens {
            body["max_tokens"] = mt.into();
        }
        if let Some(tp) = payload.top_p {
            body["top_p"] = tp.into();
        }

        if request.stream {
            self.stream_chat_sse(&url, body, request.request_id.clone()).await
        } else {
            self.unary_chat(&url, body, request.request_id.clone()).await
        }
    }

    async fn unary_chat(
        &self,
        url: &str,
        body: serde_json::Value,
        request_id: String,
    ) -> Result<ModelOutcome, HandleError> {
        let resp = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    HandleError::Timeout
                } else {
                    HandleError::UpstreamUnavailable(e.to_string())
                }
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Ok(ModelOutcome::Unary(error_response(
                &request_id,
                &format!("upstream HTTP {}: {}", status, text),
            )));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| HandleError::Internal(format!("parse upstream JSON: {}", e)))?;

        let text = json
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let model = json
            .pointer("/model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let finish_reason = json
            .pointer("/choices/0/finish_reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(ModelOutcome::Unary(ModelResponse {
            request_id,
            result: ModelResult::Completion(CompletionResult {
                text,
                reasoning_content: None,
                model,
                finish_reason,
                tool_calls: None,
                detected_intent: None,
                detected_tools: None,
                transcribed_text: None,
                speaker_id: None,
                speaker_name: None,
            }),
            metrics: None,
        }))
    }

    async fn stream_chat_sse(
        &self,
        url: &str,
        body: serde_json::Value,
        request_id: String,
    ) -> Result<ModelOutcome, HandleError> {
        use futures_util::StreamExt;

        let resp = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| HandleError::UpstreamUnavailable(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(HandleError::UpstreamUnavailable(format!(
                "upstream HTTP {}: {}",
                status, text
            )));
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<ModelStreamChunk>(64);
        let req_id = request_id.clone();

        tokio::spawn(async move {
            let mut stream = resp.bytes_stream();
            let mut buffer = String::new();
            while let Some(chunk) = stream.next().await {
                let Ok(bytes) = chunk else {
                    let _ = tx
                        .send(ModelStreamChunk {
                            request_id: req_id.clone(),
                            chunk: StreamChunkType::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: "upstream stream read error".into(),
                                details: None,
                            }),
                        })
                        .await;
                    return;
                };
                buffer.push_str(&String::from_utf8_lossy(&bytes));

                while let Some(idx) = buffer.find("\n\n") {
                    let event = buffer[..idx].to_string();
                    buffer.drain(..idx + 2);

                    for line in event.lines() {
                        let Some(data) = line.strip_prefix("data:") else {
                            continue;
                        };
                        let data = data.trim();
                        if data == "[DONE]" {
                            let _ = tx
                                .send(ModelStreamChunk {
                                    request_id: req_id.clone(),
                                    chunk: StreamChunkType::Done { final_metrics: None },
                                })
                                .await;
                            return;
                        }
                        let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
                            continue;
                        };
                        if let Some(delta) = json
                            .pointer("/choices/0/delta/content")
                            .and_then(|v| v.as_str())
                        {
                            if !delta.is_empty() {
                                let _ = tx
                                    .send(ModelStreamChunk {
                                        request_id: req_id.clone(),
                                        chunk: StreamChunkType::TextDelta(delta.to_string()),
                                    })
                                    .await;
                            }
                        }
                    }
                }
            }
        });

        Ok(ModelOutcome::Stream(rx))
    }

    async fn handle_embeddings(
        &self,
        request: &ModelRequest,
        payload: &EmbeddingsPayload,
    ) -> Result<ModelOutcome, HandleError> {
        let url = match self.api {
            UpstreamApi::OpenAi | UpstreamApi::LlamaCpp => {
                format!("{}/embeddings", self.upstream.trim_end_matches('/'))
            }
            _ => {
                return Err(HandleError::UnsupportedRequest(
                    "ten upstream API nie ma endpointu /embeddings".into(),
                ))
            }
        };

        let body = serde_json::json!({
            "model": payload.model,
            "input": payload.input,
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| HandleError::UpstreamUnavailable(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Ok(ModelOutcome::Unary(error_response(
                &request.request_id,
                &format!("upstream HTTP {}: {}", status, text),
            )));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| HandleError::Internal(format!("parse upstream JSON: {}", e)))?;

        let embeddings: Vec<Vec<f32>> = json
            .pointer("/data")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| {
                        e.pointer("/embedding").and_then(|e| e.as_array()).map(|v| {
                            v.iter()
                                .filter_map(|n| n.as_f64().map(|f| f as f32))
                                .collect()
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let model = json
            .pointer("/model")
            .and_then(|v| v.as_str())
            .unwrap_or(&payload.model)
            .to_string();

        Ok(ModelOutcome::Unary(ModelResponse {
            request_id: request.request_id.clone(),
            result: ModelResult::Embeddings(EmbeddingsResult {
                dimensions: embeddings.first().map(|e| e.len()).unwrap_or(0),
                embeddings,
                model,
            }),
            metrics: None,
        }))
    }
}

#[async_trait]
impl ModelHandler for ReverseProxyHandler {
    async fn handle(&self, request: ModelRequest) -> Result<ModelOutcome, HandleError> {
        match &request.payload {
            ModelPayload::Completion(p) => self.handle_chat(&request, p).await,
            ModelPayload::Embeddings(p) => self.handle_embeddings(&request, p).await,
            other => Err(HandleError::UnsupportedRequest(format!(
                "payload {:?} nie obslugiwany przez ReverseProxy",
                std::mem::discriminant(other)
            ))),
        }
    }
}

fn error_response(request_id: &str, message: &str) -> ModelResponse {
    ModelResponse {
        request_id: request_id.to_string(),
        result: ModelResult::Error(ErrorInfo {
            error_type: ErrorType::InternalError,
            message: message.to_string(),
            details: None,
        }),
        metrics: None,
    }
}
