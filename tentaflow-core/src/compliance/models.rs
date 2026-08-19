// =============================================================================
// Plik: compliance/models.rs
// Opis: Typy domenowe Compliance Core dla retencji, ROPA i AI audit.
// =============================================================================

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComplianceRiskClass {
    Low,
    Standard,
    High,
    Critical,
}

impl ComplianceRiskClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Standard => "standard",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "low" => Some(Self::Low),
            "standard" => Some(Self::Standard),
            "high" => Some(Self::High),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetentionScopeKind {
    Audit,
    AiAudit,
    DataCategory,
    Document,
    Dsar,
    Breach,
    General,
    AgentRuns,
    Events,
}

impl RetentionScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audit => "audit",
            Self::AiAudit => "ai_audit",
            Self::DataCategory => "data_category",
            Self::Document => "document",
            Self::Dsar => "dsar",
            Self::Breach => "breach",
            Self::General => "general",
            Self::AgentRuns => "agent_runs",
            Self::Events => "events",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "audit" => Some(Self::Audit),
            "ai_audit" => Some(Self::AiAudit),
            "data_category" => Some(Self::DataCategory),
            "document" => Some(Self::Document),
            "dsar" => Some(Self::Dsar),
            "breach" => Some(Self::Breach),
            "general" => Some(Self::General),
            "agent_runs" => Some(Self::AgentRuns),
            "events" => Some(Self::Events),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiEventStatus {
    Running,
    Success,
    Failed,
    Cancelled,
}

impl AiEventStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "success" => Some(Self::Success),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiPayloadKind {
    Prompt,
    Response,
    System,
    ToolInput,
    ToolOutput,
}

impl AiPayloadKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Response => "response",
            Self::System => "system",
            Self::ToolInput => "tool_input",
            Self::ToolOutput => "tool_output",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiSourceKind {
    Rag,
    File,
    Url,
    Database,
    Addon,
    Memory,
    Vector,
    Other,
}

impl AiSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rag => "rag",
            Self::File => "file",
            Self::Url => "url",
            Self::Database => "database",
            Self::Addon => "addon",
            Self::Memory => "memory",
            Self::Vector => "vector",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolCallStatus {
    Running,
    Success,
    Failed,
}

impl ToolCallStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Success => "success",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceDataCategory {
    pub category_id: String,
    pub org_id: String,
    pub slug: String,
    pub name_translations: String,
    pub description_translations: String,
    pub personal_data: bool,
    pub sensitive_data: bool,
    pub risk_class: ComplianceRiskClass,
    pub source_scope: String,
    pub addon_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceRetentionPolicy {
    pub retention_policy_id: String,
    pub org_id: String,
    pub slug: String,
    pub name_translations: String,
    pub scope_kind: RetentionScopeKind,
    pub category_id: Option<String>,
    pub retention_days: i64,
    pub minimum_days: i64,
    pub action_after_retention: String,
    pub is_default: bool,
    pub is_active: bool,
}

#[derive(Debug, Clone)]
pub struct NewAiEvent<'a> {
    pub org_id: &'a str,
    pub user_id: Option<&'a str>,
    pub node_id: &'a str,
    pub addon_id: Option<&'a str>,
    pub instance_id: Option<&'a str>,
    pub flow_id: Option<&'a str>,
    pub flow_node_id: Option<&'a str>,
    pub agent_id: Option<&'a str>,
    pub agent_run_id: Option<&'a str>,
    pub request_id: &'a str,
    /// Cross-event correlation key (§3.4). The session/root event seeds it with
    /// its own `request_id`; per-call events of the same user turn copy that
    /// value, so one turn's rows link despite distinct `request_id`s.
    pub correlation_id: Option<&'a str>,
    pub model_id: &'a str,
    pub backend: &'a str,
    pub risk_class: ComplianceRiskClass,
    pub legal_basis_id: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceAiEvent {
    pub event_id: String,
    pub org_id: String,
    pub user_id: Option<String>,
    pub node_id: String,
    pub addon_id: Option<String>,
    pub instance_id: Option<String>,
    pub flow_id: Option<String>,
    pub flow_node_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_run_id: Option<String>,
    pub request_id: String,
    pub model_id: String,
    pub backend: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: AiEventStatus,
    pub risk_class: ComplianceRiskClass,
    pub legal_basis_id: Option<String>,
    pub retention_policy_id: String,
    pub prompt_hash: String,
    pub response_hash: String,
    pub audit_log_id: Option<i64>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AiEventListFilter {
    pub status: Option<AiEventStatus>,
    pub user_id: Option<String>,
    pub addon_id: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Clone)]
pub struct NewAiPayload<'a> {
    pub event_id: &'a str,
    pub payload_kind: AiPayloadKind,
    pub content_text: &'a str,
    pub content_redacted: bool,
    pub token_count: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewAiSource<'a> {
    pub event_id: &'a str,
    pub source_kind: AiSourceKind,
    pub source_ref: &'a str,
    pub source_text: &'a str,
    pub title: &'a str,
    pub excerpt_text: &'a str,
    pub score: Option<f64>,
    pub metadata_cbor: &'a [u8],
}

#[derive(Debug, Clone)]
pub struct NewAiToolCall<'a> {
    pub event_id: &'a str,
    /// Model-issued call id (`LlmToolCall.id`). `None` for rows that only
    /// record the model's request without an execution to pair it with.
    pub llm_tool_call_id: Option<&'a str>,
    pub addon_id: Option<&'a str>,
    pub tool_name: &'a str,
    pub input_text: &'a str,
    pub output_text: &'a str,
    pub status: ToolCallStatus,
    pub error_message: Option<&'a str>,
    /// Real execution start (UTC, `%Y-%m-%dT%H:%M:%SZ`). `None` falls back
    /// to the insert timestamp — correct for request-only rows.
    pub started_at: Option<&'a str>,
}
