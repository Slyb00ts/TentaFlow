// ===== File: benchmark/types.rs — Benchmark Studio data types: config, targets, samples, DB records =====

use std::net::{IpAddr, Ipv6Addr};

use serde::{Deserialize, Serialize};

/// Enabled scenarios + their parameters, stored as `benchmarks.config_json`.
/// Every scenario is optional; a disabled scenario is simply absent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkConfig {
    #[serde(default)]
    pub latency: Option<LatencyConfig>,
    #[serde(default)]
    pub throughput: Option<ThroughputConfig>,
    #[serde(default)]
    pub context: Option<ContextConfig>,
    #[serde(default)]
    pub sustained: Option<SustainedConfig>,
    /// Synthetic prompt size (approximate; real prompt_tokens come from usage).
    #[serde(default = "default_prompt_tokens")]
    pub prompt_tokens: u32,
    /// Generation budget per request (`max_tokens`).
    #[serde(default = "default_gen_tokens")]
    pub gen_tokens: u32,
    /// Per-request hard deadline; an elapsed timeout becomes an error sample.
    #[serde(default = "default_timeout_secs")]
    pub request_timeout_secs: u64,
}

fn default_prompt_tokens() -> u32 {
    512
}
fn default_gen_tokens() -> u32 {
    128
}
fn default_timeout_secs() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyConfig {
    /// Measured repetitions (an extra warmup request is not counted).
    #[serde(default = "default_latency_repeats")]
    pub repeats: u32,
}

fn default_latency_repeats() -> u32 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThroughputConfig {
    /// Concurrency levels swept in order, e.g. [1, 4, 16, 64].
    #[serde(default = "default_concurrency_levels")]
    pub levels: Vec<u32>,
    /// Sequential requests per worker at each level (total = level × this).
    #[serde(default = "default_requests_per_worker")]
    pub requests_per_worker: u32,
}

fn default_concurrency_levels() -> Vec<u32> {
    vec![1, 4, 16, 64]
}
fn default_requests_per_worker() -> u32 {
    4
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Prompt lengths swept in order, e.g. [128, 2048, 8192, 32768].
    #[serde(default = "default_context_lengths")]
    pub prompt_lengths: Vec<u32>,
    /// Measured repetitions per length (plus one uncounted warmup).
    #[serde(default = "default_context_repeats")]
    pub repeats: u32,
}

fn default_context_lengths() -> Vec<u32> {
    vec![128, 2048, 8192, 32768]
}
fn default_context_repeats() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SustainedConfig {
    #[serde(default = "default_sustained_minutes")]
    pub minutes: u32,
    #[serde(default = "default_sustained_concurrency")]
    pub concurrency: u32,
}

fn default_sustained_minutes() -> u32 {
    10
}
fn default_sustained_concurrency() -> u32 {
    8
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiType {
    OpenAi,
    Anthropic,
    /// In-process: the request goes through `ModelRuntimeExecutor::stream_chat`
    /// instead of a socket. This is the only way to measure a backend that has
    /// no dialable chat endpoint — embedded llama.cpp / MLX, a QUIC sidecar, a
    /// coding-agent bridge — and it also reaches models owned by another mesh
    /// node through the normal forward path.
    Local,
}

impl ApiType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiType::OpenAi => "openai",
            ApiType::Anthropic => "anthropic",
            ApiType::Local => "local",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "openai" => Some(ApiType::OpenAi),
            "anthropic" => Some(ApiType::Anthropic),
            "local" => Some(ApiType::Local),
            _ => None,
        }
    }
}

/// Runtime target handed to the client: endpoint resolved, API key decrypted.
#[derive(Debug, Clone)]
pub struct TargetSpec {
    pub id: String,
    pub label: String,
    pub api: ApiType,
    /// Either a full URL (`http(s)://host[:port]`) or a bare hostname
    /// combined with `port` (443 implies https). Empty for `ApiType::Local`,
    /// which dispatches in-process and never opens a socket of its own.
    pub host: String,
    pub port: u16,
    pub api_key: Option<String>,
    pub model: String,
}

impl TargetSpec {
    /// Base URL without an API path. A `host` that already carries a scheme is
    /// trusted verbatim (service endpoint_url snapshots arrive that way).
    pub fn base_url(&self) -> String {
        if self.host.contains("://") {
            self.host.trim_end_matches('/').to_string()
        } else {
            let scheme = if self.port == 443 { "https" } else { "http" };
            format!("{}://{}:{}", scheme, self.host, self.port)
        }
    }

    /// Pre-flight check for one target. An in-process target carries no
    /// endpoint at all — the model name IS the address, resolved through the
    /// catalog — so only that is required. For HTTP targets we reject
    /// cloud-metadata / link-local destinations. Private, loopback and LAN
    /// targets are intentionally allowed: benchmarking a local inference
    /// service hits loopback/LAN by design, exactly like an addon exact-host
    /// network rule. The admin-permission gate for who may run a benchmark lives
    /// in the Chunk 2 handler; here we only stop a target from being pointed at
    /// the instance metadata endpoint (which could leak cloud IAM credentials).
    /// Only literal IPs are checked — DNS names are not an SSRF vector here
    /// because the destination is admin-configured, not attacker-supplied per
    /// request.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.model.trim().is_empty() {
            anyhow::bail!("target has no model name");
        }
        if self.api == ApiType::Local {
            return Ok(());
        }
        if self.host.trim().is_empty() {
            anyhow::bail!("target has no host");
        }
        let host = self.host_only();
        if let Ok(ip) = host.parse::<IpAddr>() {
            let blocked = match ip {
                // 169.254.0.0/16 (link-local) covers the 169.254.169.254 metadata IP.
                IpAddr::V4(v4) => {
                    let o = v4.octets();
                    o[0] == 169 && o[1] == 254
                }
                // IPv6 cloud metadata endpoint (fd00:ec2::254).
                IpAddr::V6(v6) => v6 == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254),
            };
            if blocked {
                anyhow::bail!(
                    "target host {host} is a cloud-metadata/link-local address (blocked)"
                );
            }
        }
        Ok(())
    }

    /// Bare host of the target: strips an optional scheme, path and `:port` /
    /// `[ipv6]:port` wrapper so `validate` can parse a literal IP.
    fn host_only(&self) -> String {
        let without_scheme = self
            .host
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(&self.host);
        let host_port = without_scheme
            .split(['/', '?', '#'])
            .next()
            .unwrap_or(without_scheme);
        if let Some(rest) = host_port.strip_prefix('[') {
            if let Some(end) = rest.find(']') {
                return rest[..end].to_string();
            }
        }
        match host_port.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') && p.chars().all(|c| c.is_ascii_digit()) => {
                h.to_string()
            }
            _ => host_port.to_string(),
        }
    }
}

/// One measured request. Timing is client-observed (dispatch → chunk arrival);
/// token counts come exclusively from the API `usage` payload, never estimated,
/// so throughput fields are `None` when the backend does not report usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestSample {
    pub ttft_ms: f64,
    pub prefill_tps: Option<f64>,
    pub decode_tps: Option<f64>,
    pub total_ms: f64,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    #[serde(default)]
    pub error: Option<String>,
}

impl RequestSample {
    pub fn failed(elapsed_ms: f64, error: String) -> Self {
        Self {
            ttft_ms: 0.0,
            prefill_tps: None,
            decode_tps: None,
            total_ms: elapsed_ms,
            prompt_tokens: 0,
            completion_tokens: 0,
            error: Some(error),
        }
    }
}

/// Progress events emitted by the runner through a generic callback.
/// Chunk 2 bridges these onto the binary protocol / log bus.
#[derive(Debug, Clone)]
pub enum BenchEvent {
    Phase {
        target_id: String,
        target_label: String,
        scenario: &'static str,
        message: String,
    },
    Log {
        line: String,
    },
    PartialResult {
        result: BenchmarkResultRecord,
    },
    Done,
    Error {
        message: String,
    },
}

// --- DB records (rows of the benchmark_* tables) ---

#[derive(Debug, Clone)]
pub struct BenchmarkRecord {
    pub id: String,
    pub org_id: String,
    pub name: String,
    pub config_json: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct BenchmarkTargetRecord {
    pub id: String,
    pub benchmark_id: String,
    /// 'service' | 'external'.
    pub kind: String,
    /// For service targets: opaque service reference (`service_id` or `model@node`).
    pub service_ref: Option<String>,
    /// 'openai' | 'anthropic'.
    pub api_type: String,
    pub host: String,
    pub port: u16,
    /// Encrypted at rest with the settings cipher; decrypted only at run time.
    pub api_key_enc: Option<String>,
    pub model: String,
    pub label: String,
}

/// Upsert payload for a target. `api_key: None` keeps an already stored key
/// (the dashboard sends targets back without re-entering secrets).
#[derive(Debug, Clone)]
pub struct BenchmarkTargetUpsert {
    pub id: String,
    pub kind: String,
    pub service_ref: Option<String>,
    pub api_type: String,
    pub host: String,
    pub port: u16,
    pub api_key: Option<String>,
    pub model: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct BenchmarkRunRecord {
    pub id: String,
    pub benchmark_id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// 'running' | 'success' | 'failed' | 'cancelled'.
    pub status: String,
    pub error: Option<String>,
    pub engine_meta_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkResultRecord {
    pub id: String,
    pub run_id: String,
    pub target_id: String,
    /// Label snapshot: results stay renderable after target edits/deletion.
    pub target_label: String,
    /// 'latency' | 'throughput' | 'context' | 'sustained'.
    pub scenario: String,
    pub variant_json: String,
    pub ttft_ms_mean: Option<f64>,
    pub ttft_ms_sigma: Option<f64>,
    pub prefill_tps_mean: Option<f64>,
    pub prefill_tps_sigma: Option<f64>,
    pub decode_tps_mean: Option<f64>,
    pub decode_tps_sigma: Option<f64>,
    pub total_ms_mean: Option<f64>,
    pub total_ms_sigma: Option<f64>,
    pub p50_ms: Option<f64>,
    pub p90_ms: Option<f64>,
    pub p99_ms: Option<f64>,
    pub requests: u32,
    pub errors: u32,
    pub samples_json: String,
}

/// List item for the benchmark overview: definition + latest run summary.
#[derive(Debug, Clone)]
pub struct BenchmarkListItem {
    pub record: BenchmarkRecord,
    pub target_count: u32,
    pub models: Vec<String>,
    pub last_run: Option<BenchmarkRunRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(api: ApiType, host: &str, model: &str) -> TargetSpec {
        TargetSpec {
            id: "t1".into(),
            label: "target".into(),
            api,
            host: host.into(),
            port: 8080,
            api_key: None,
            model: model.into(),
        }
    }

    #[test]
    fn api_type_round_trips_through_storage() {
        for api in [ApiType::OpenAi, ApiType::Anthropic, ApiType::Local] {
            assert_eq!(ApiType::parse(api.as_str()), Some(api));
        }
        assert_eq!(ApiType::parse("grpc"), None);
    }

    #[test]
    fn local_target_needs_a_model_but_no_host() {
        spec(ApiType::Local, "", "qwen3.5-0.8b")
            .validate()
            .expect("model name is the whole address of an in-process target");
        assert!(spec(ApiType::Local, "", "").validate().is_err());
    }

    #[test]
    fn http_target_needs_a_host() {
        assert!(spec(ApiType::OpenAi, "", "gpt-5.1").validate().is_err());
        spec(ApiType::OpenAi, "10.0.4.21", "gpt-5.1")
            .validate()
            .expect("LAN destinations are allowed on purpose");
    }

    #[test]
    fn cloud_metadata_destination_stays_blocked() {
        assert!(spec(ApiType::OpenAi, "169.254.169.254", "m")
            .validate()
            .is_err());
        assert!(spec(ApiType::OpenAi, "http://[fd00:ec2::254]:80", "m")
            .validate()
            .is_err());
    }
}
