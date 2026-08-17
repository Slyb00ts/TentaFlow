// ===== File: benchmark/client.rs — streaming benchmark clients (OpenAI-compatible + Anthropic over HTTP, plus the in-process path) with client-side TTFT/decode timing =====

use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::{json, Value};

use super::local::{LocalRunner, RouteNote};
use super::types::{ApiType, RequestSample, TargetSpec};

/// Dispatcher for all benchmark requests. Cloning is cheap (reqwest clients
/// share their connection pool, the local runner is one `Arc`), which the
/// concurrency scenarios rely on.
#[derive(Clone)]
pub struct BenchClient {
    http: reqwest::Client,
    /// In-process path. `None` only when the runtime executor is not wired
    /// (tests, a Router built without init) — an in-process target then fails
    /// with a clear error instead of silently degrading to HTTP.
    local: Option<LocalRunner>,
}

/// Raw observation of one streamed response. Timestamps are taken at chunk
/// ARRIVAL inside the drain loop (same principle as ExternalPerfStream): the
/// benchmark client is the consumer and reads eagerly, so the decode window
/// reflects the server's generation pace, not consumption pace. Shared by the
/// HTTP and in-process paths so both produce numbers on the same scale.
pub(super) struct StreamObservation {
    pub(super) first_token_at: Option<Instant>,
    pub(super) last_token_at: Option<Instant>,
    /// End of the response stream (bytes exhausted / `[DONE]` / `message_stop`),
    /// stamped after draining. total_ms measures to here, not to the last content
    /// chunk, so trailing usage-only frames and stream teardown are included.
    pub(super) stream_end_at: Option<Instant>,
    pub(super) prompt_tokens: u32,
    pub(super) completion_tokens: u32,
    pub(super) usage_seen: bool,
}

impl StreamObservation {
    pub(super) fn empty() -> Self {
        Self {
            first_token_at: None,
            last_token_at: None,
            stream_end_at: None,
            prompt_tokens: 0,
            completion_tokens: 0,
            usage_seen: false,
        }
    }
}

impl BenchClient {
    pub fn new(local: Option<LocalRunner>) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self { http, local })
    }

    /// Executes one streamed request and turns the observation into a sample.
    /// TTFT = dispatch → first content chunk; total = dispatch → last content
    /// chunk; prefill t/s = prompt_tokens / TTFT; decode t/s uses N-1 intervals
    /// between the first and last token (matching build_external_perf).
    /// Token counts come ONLY from the response usage — never estimated.
    pub async fn execute(
        &self,
        target: &TargetSpec,
        prompt: &str,
        max_tokens: u32,
        timeout: Duration,
    ) -> RequestSample {
        self.execute_routed(target, prompt, max_tokens, timeout)
            .await
            .0
    }

    /// Same as `execute`, but also reports which backend served an in-process
    /// request. HTTP targets have a fixed address, so their route is empty.
    pub async fn execute_routed(
        &self,
        target: &TargetSpec,
        prompt: &str,
        max_tokens: u32,
        timeout: Duration,
    ) -> (RequestSample, RouteNote) {
        let start = Instant::now();
        let fut = async {
            match target.api {
                ApiType::OpenAi => self
                    .stream_openai(target, prompt, max_tokens)
                    .await
                    .map(|obs| (obs, RouteNote::default())),
                ApiType::Anthropic => self
                    .stream_anthropic(target, prompt, max_tokens)
                    .await
                    .map(|obs| (obs, RouteNote::default())),
                ApiType::Local => match &self.local {
                    Some(runner) => runner.stream(&target.model, prompt, max_tokens).await,
                    None => anyhow::bail!(
                        "in-process target requires the model runtime executor, which is not wired"
                    ),
                },
            }
        };
        match tokio::time::timeout(timeout, fut).await {
            Ok(Ok((obs, route))) => (build_sample(start, &obs), route),
            Ok(Err(e)) => (
                RequestSample::failed(start.elapsed().as_secs_f64() * 1000.0, e.to_string()),
                RouteNote::default(),
            ),
            Err(_) => (
                RequestSample::failed(
                    start.elapsed().as_secs_f64() * 1000.0,
                    format!("request timeout after {}s", timeout.as_secs()),
                ),
                RouteNote::default(),
            ),
        }
    }

    /// OpenAI-compatible: POST /v1/chat/completions, stream:true. Usage arrives
    /// on the final chunk (vLLM/sglang usage-tail); `stream_options.include_usage`
    /// asks upstream OpenAI-style servers to emit it.
    async fn stream_openai(
        &self,
        target: &TargetSpec,
        prompt: &str,
        max_tokens: u32,
    ) -> anyhow::Result<StreamObservation> {
        let base = target.base_url();
        // Service endpoint snapshots sometimes already end in /v1.
        let url = if base.ends_with("/v1") {
            format!("{}/chat/completions", base)
        } else {
            format!("{}/v1/chat/completions", base)
        };
        let make_body = |include_usage: bool| {
            let mut body = json!({
                "model": target.model,
                "messages": [{"role": "user", "content": prompt}],
                "max_tokens": max_tokens,
                "temperature": 0.0,
                "stream": true,
            });
            if include_usage {
                body["stream_options"] = json!({"include_usage": true});
            }
            body
        };

        let response = {
            let resp = self.send_openai(&url, target, &make_body(true)).await?;
            let status = resp.status();
            // Older OpenAI-compatible servers reject the unknown `stream_options`
            // field with 400/422. Retry once without it: the stream then works and
            // usage-derived throughput (prefill/decode t/s) simply stays None.
            if status == reqwest::StatusCode::BAD_REQUEST
                || status == reqwest::StatusCode::UNPROCESSABLE_ENTITY
            {
                drop(resp);
                self.send_openai(&url, target, &make_body(false)).await?
            } else {
                resp
            }
        };
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {}: {}", status, truncate(&body, 300));
        }

        let mut obs = StreamObservation::empty();
        drain_sse(response, |payload| {
            if payload == "[DONE]" {
                return Ok(());
            }
            let chunk: Value = serde_json::from_str(payload)?;
            // Reasoning tokens are decode work too (see ExternalPerfStream):
            // chain-of-thought models stream reasoning_content before content.
            let has_content = chunk["choices"].as_array().is_some_and(|choices| {
                choices.iter().any(|c| {
                    c["delta"]["content"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
                        || c["delta"]["reasoning_content"]
                            .as_str()
                            .is_some_and(|s| !s.is_empty())
                })
            });
            if has_content {
                let now = Instant::now();
                if obs.first_token_at.is_none() {
                    obs.first_token_at = Some(now);
                }
                obs.last_token_at = Some(now);
            }
            if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
                obs.prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
                obs.completion_tokens = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;
                obs.usage_seen = true;
            }
            Ok(())
        })
        .await?;
        obs.stream_end_at = Some(Instant::now());
        Ok(obs)
    }

    /// Sends one OpenAI-compatible request; auth header applied when a key is set.
    async fn send_openai(
        &self,
        url: &str,
        target: &TargetSpec,
        body: &Value,
    ) -> anyhow::Result<reqwest::Response> {
        let mut req = self
            .http
            .post(url)
            .header("Content-Type", "application/json")
            .json(body);
        if let Some(key) = &target.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        Ok(req.send().await?)
    }

    /// Anthropic Messages API: POST /v1/messages, stream:true. input_tokens
    /// arrive in `message_start`, output_tokens in `message_delta`; content
    /// arrival is `content_block_delta` with a non-empty text delta.
    async fn stream_anthropic(
        &self,
        target: &TargetSpec,
        prompt: &str,
        max_tokens: u32,
    ) -> anyhow::Result<StreamObservation> {
        let url = format!("{}/v1/messages", target.base_url());
        let body = json!({
            "model": target.model,
            "max_tokens": max_tokens,
            "messages": [{"role": "user", "content": prompt}],
            "stream": true,
        });
        let mut req = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .json(&body);
        if let Some(key) = &target.api_key {
            req = req.header("x-api-key", key);
        }
        let response = req.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {}: {}", status, truncate(&body, 300));
        }

        let mut obs = StreamObservation::empty();
        let mut api_error: Option<String> = None;
        drain_sse(response, |payload| {
            let event: Value = serde_json::from_str(payload)?;
            match event["type"].as_str().unwrap_or("") {
                "message_start" => {
                    if let Some(v) = event["message"]["usage"]["input_tokens"].as_u64() {
                        obs.prompt_tokens = v as u32;
                        obs.usage_seen = true;
                    }
                }
                "content_block_delta" => {
                    let has_text = event["delta"]["text"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
                        || event["delta"]["thinking"]
                            .as_str()
                            .is_some_and(|s| !s.is_empty());
                    if has_text {
                        let now = Instant::now();
                        if obs.first_token_at.is_none() {
                            obs.first_token_at = Some(now);
                        }
                        obs.last_token_at = Some(now);
                    }
                }
                "message_delta" => {
                    if let Some(v) = event["usage"]["output_tokens"].as_u64() {
                        obs.completion_tokens = v as u32;
                        obs.usage_seen = true;
                    }
                }
                "error" => {
                    api_error = Some(
                        event["error"]["message"]
                            .as_str()
                            .unwrap_or("anthropic stream error")
                            .to_string(),
                    );
                }
                _ => {}
            }
            Ok(())
        })
        .await?;
        obs.stream_end_at = Some(Instant::now());
        if let Some(e) = api_error {
            anyhow::bail!("{}", e);
        }
        Ok(obs)
    }
}

/// Reads the SSE body eagerly, invoking `on_data` with each event's payload as
/// it arrives so chunk timestamps are stamped at arrival, never at consumption.
///
/// Parses per SSE event (a block terminated by a blank line), not per line:
/// multiple `data:` lines in one event are concatenated with `\n`, both `data:`
/// and `data: ` prefixes are accepted, and a trailing event with no final blank
/// line is flushed at EOF. `[DONE]` is forwarded verbatim for the caller.
async fn drain_sse(
    response: reqwest::Response,
    mut on_data: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut data_lines: Vec<String> = Vec::new();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        match std::str::from_utf8(&bytes) {
            Ok(s) => buffer.push_str(s),
            Err(_) => buffer.push_str(&String::from_utf8_lossy(&bytes)),
        }
        while let Some(newline) = buffer.find('\n') {
            let line: String = buffer[..newline].trim_end_matches('\r').to_string();
            buffer.drain(..=newline);
            if line.is_empty() {
                flush_event(&mut data_lines, &mut on_data)?;
            } else if let Some(rest) = line.strip_prefix("data:") {
                // SSE strips a single optional leading space after the colon.
                data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
            // Other fields (event:, id:, retry:, comments) are irrelevant here.
        }
    }
    // Tail without a trailing newline (EOF mid-line) is still a data field.
    let tail = buffer.trim_end_matches('\r');
    if let Some(rest) = tail.strip_prefix("data:") {
        data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
    }
    flush_event(&mut data_lines, &mut on_data)?;
    Ok(())
}

/// Joins the buffered `data:` lines of one event and dispatches the payload.
fn flush_event(
    data_lines: &mut Vec<String>,
    on_data: &mut impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if data_lines.is_empty() {
        return Ok(());
    }
    let payload = data_lines.join("\n");
    data_lines.clear();
    if !payload.is_empty() {
        on_data(&payload)?;
    }
    Ok(())
}

fn build_sample(start: Instant, obs: &StreamObservation) -> RequestSample {
    let Some(first) = obs.first_token_at else {
        return RequestSample::failed(
            start.elapsed().as_secs_f64() * 1000.0,
            "stream ended without any content token".to_string(),
        );
    };
    let last = obs.last_token_at.unwrap_or(first);
    let ttft_ms = first.duration_since(start).as_secs_f64() * 1000.0;
    // total = dispatch → end of stream (usage tail + teardown), falling back to
    // the last content chunk if the stream end was never stamped.
    let total_end = obs.stream_end_at.unwrap_or(last).max(last);
    let total_ms = total_end.duration_since(start).as_secs_f64() * 1000.0;

    // prefill t/s = real prompt_tokens over the window up to the first token —
    // an honest approximation (network + queue + server prefill).
    let prefill_tps = if obs.usage_seen && obs.prompt_tokens > 0 && ttft_ms > 0.0 {
        Some(obs.prompt_tokens as f64 / (ttft_ms / 1000.0))
    } else {
        None
    };

    // decode t/s = (N-1) intervals over the first→last token window; a single
    // token has no window, so decode stays unknown rather than fabricated.
    let decode_tps = if obs.usage_seen && obs.completion_tokens > 1 {
        let window = last.duration_since(first).as_secs_f64();
        if window > 0.0 {
            Some((obs.completion_tokens - 1) as f64 / window)
        } else {
            None
        }
    } else {
        None
    };

    RequestSample {
        ttft_ms,
        prefill_tps,
        decode_tps,
        total_ms,
        prompt_tokens: obs.prompt_tokens,
        completion_tokens: obs.completion_tokens,
        error: None,
    }
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}
