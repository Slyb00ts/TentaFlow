// =============================================================================
// Plik: compliance/ai_gateway.rs
// Opis: Centralna brama AI audit zapisująca prompty, odpowiedzi i tool calls.
// Przykład: AiGateway::start_chat_event(&request, user, context)?;
// =============================================================================

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::api::openai::types::{
    ChatCompletionRequest, ChatCompletionResponse, ContentPart, MessageContent, ToolCall, Usage,
};
use crate::auth::acl::UserContext;
use crate::db::DbPool;

use super::models::{
    AiEventStatus, AiPayloadKind, ComplianceRiskClass, NewAiEvent, NewAiPayload, NewAiToolCall,
    ToolCallStatus,
};
use super::repository::{
    add_ai_payload, add_ai_tool_call, default_ai_legal_basis_id, finish_ai_event, start_ai_event,
};

#[derive(Debug, Clone, Default)]
pub struct AiGatewayContext {
    pub org_id: Option<String>,
    pub addon_id: Option<String>,
    pub instance_id: Option<String>,
    pub flow_id: Option<String>,
    pub flow_node_id: Option<String>,
    /// Agent definition driving this call (Harness §3.1). Set by the
    /// gateway-aware LlmDispatcherImpl from flow variables the harness writes.
    pub agent_id: Option<String>,
    /// Agent run this call belongs to — the correlation key that stitches every
    /// `compliance_ai_events` row of one harness run together (§3.4).
    pub agent_run_id: Option<String>,
    /// Cross-event correlation key for a single user turn (§3.4). The routing
    /// layer seeds it with the session event's `request_id`; the flow's per-call
    /// `llm` events copy that value here so all rows of one turn share it. When
    /// `None`, a started event becomes its own anchor (its `request_id`).
    pub correlation_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AiGateway {
    db: DbPool,
    node_id: String,
}

#[derive(Debug, Clone)]
pub struct AiEventHandle {
    db: DbPool,
    event_id: String,
    /// The minted `request_id` of this event. Routing uses the session event's
    /// value as the turn's correlation key so the flow's per-call events copy it
    /// (§3.4).
    request_id: String,
}

/// One EXECUTED tool call (HARNESS_PLAN §3.1) — the real outcome of running
/// a tool, as opposed to the model's request rows written at finish. Payloads
/// are hashed by the repository; only hashes reach the table.
#[derive(Debug, Clone)]
pub struct ToolExecution<'a> {
    /// Model-issued call id (`LlmToolCall.id`).
    pub tool_call_id: &'a str,
    /// Owning addon; `None` for core built-in tools.
    pub addon_id: Option<&'a str>,
    /// Public tool name (`"addon_id.tool_name"`).
    pub tool_name: &'a str,
    /// Arguments JSON as handed to the tool.
    pub arguments: &'a str,
    /// Tool output JSON (or error payload returned to the model).
    pub output: &'a str,
    pub success: bool,
    pub error_message: Option<&'a str>,
    /// Execution start; the finish instant is the recording time.
    pub started_at: chrono::DateTime<chrono::Utc>,
}

impl AiGateway {
    pub fn new(db: DbPool, node_id: impl Into<String>) -> Self {
        Self {
            db,
            node_id: node_id.into(),
        }
    }

    pub fn start_chat_event(
        &self,
        request: &ChatCompletionRequest,
        user: Option<&UserContext>,
        context: Option<&AiGatewayContext>,
    ) -> Result<AiEventHandle> {
        let conn = self.db.write().map_err(|_| anyhow!("blokada DB zatruta"))?;
        let org_id = resolve_org_id(&conn, user, context)?;
        let request_id = uuid::Uuid::new_v4().to_string();
        // A session/root event with no inbound correlation key anchors the turn:
        // it correlates to its own request_id, which routing then propagates so
        // the flow's per-call events copy it (§3.4).
        let correlation_id = context
            .and_then(|c| c.correlation_id.as_deref())
            .unwrap_or(request_id.as_str());
        let legal_basis_id = default_ai_legal_basis_id(&org_id);
        let event_id = start_ai_event(
            &conn,
            &NewAiEvent {
                org_id: &org_id,
                user_id: user.map(|u| u.user_id.as_str()),
                node_id: &self.node_id,
                addon_id: context.and_then(|c| c.addon_id.as_deref()),
                instance_id: context.and_then(|c| c.instance_id.as_deref()),
                flow_id: context.and_then(|c| c.flow_id.as_deref()),
                flow_node_id: context.and_then(|c| c.flow_node_id.as_deref()),
                agent_id: context.and_then(|c| c.agent_id.as_deref()),
                agent_run_id: context.and_then(|c| c.agent_run_id.as_deref()),
                request_id: &request_id,
                correlation_id: Some(correlation_id),
                model_id: &request.model,
                backend: "chat",
                risk_class: ComplianceRiskClass::High,
                legal_basis_id: Some(&legal_basis_id),
            },
        )?;
        add_ai_payload(
            &conn,
            &NewAiPayload {
                event_id: &event_id,
                payload_kind: AiPayloadKind::Prompt,
                content_text: &chat_request_prompt_text(request),
                content_redacted: false,
                token_count: None,
            },
        )?;
        Ok(AiEventHandle {
            db: self.db.clone(),
            event_id,
            request_id,
        })
    }

    /// Records one EXECUTED tool call against the latest `compliance_ai_events`
    /// row of an agent run (the LLM call that requested it — same
    /// `agent_run_id`). The tool_exec block calls this after running each tool;
    /// it pairs the model-issued call id with the real execution status, output
    /// hash and timing (§3.10). Returns `Ok(None)` when the run has no event yet
    /// (audit then no-ops — never blocks the tool loop).
    pub fn record_run_tool_execution(
        &self,
        agent_run_id: &str,
        execution: &ToolExecution<'_>,
    ) -> Result<Option<String>> {
        let conn = self.db.write().map_err(|_| anyhow!("blokada DB zatruta"))?;
        let Some(event_id) =
            super::repository::latest_ai_event_id_for_run(&conn, agent_run_id)?
        else {
            return Ok(None);
        };
        let started_at = execution
            .started_at
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let tool_call_id = add_ai_tool_call(
            &conn,
            &NewAiToolCall {
                event_id: &event_id,
                llm_tool_call_id: Some(execution.tool_call_id),
                addon_id: execution.addon_id,
                tool_name: execution.tool_name,
                input_text: execution.arguments,
                output_text: execution.output,
                status: if execution.success {
                    ToolCallStatus::Success
                } else {
                    ToolCallStatus::Failed
                },
                error_message: execution.error_message,
                started_at: Some(&started_at),
            },
        )?;
        Ok(Some(tool_call_id))
    }
}

impl AiEventHandle {
    pub fn event_id(&self) -> &str {
        &self.event_id
    }

    /// This event's `request_id`. Routing seeds the turn's `correlation_id` with
    /// the session event's value (§3.4).
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Records the result of one executed tool call into
    /// `compliance_ai_tool_calls`. Called by the tool loop right after the
    /// tool returns — pairs the model-issued call id with the real status,
    /// output hash and timing. Returns the row's UUID `tool_call_id`.
    pub fn record_tool_execution(&self, execution: &ToolExecution<'_>) -> Result<String> {
        let conn = self.db.write().map_err(|_| anyhow!("DB lock poisoned"))?;
        let started_at = execution
            .started_at
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        add_ai_tool_call(
            &conn,
            &NewAiToolCall {
                event_id: &self.event_id,
                llm_tool_call_id: Some(execution.tool_call_id),
                addon_id: execution.addon_id,
                tool_name: execution.tool_name,
                input_text: execution.arguments,
                output_text: execution.output,
                status: if execution.success {
                    ToolCallStatus::Success
                } else {
                    ToolCallStatus::Failed
                },
                error_message: execution.error_message,
                started_at: Some(&started_at),
            },
        )
    }

    pub fn finish_success(&self, response: &ChatCompletionResponse) -> Result<()> {
        let conn = self.db.write().map_err(|_| anyhow!("blokada DB zatruta"))?;
        let response_text = chat_response_text(response);
        add_ai_payload(
            &conn,
            &NewAiPayload {
                event_id: &self.event_id,
                payload_kind: AiPayloadKind::Response,
                content_text: &response_text,
                content_redacted: false,
                token_count: response
                    .usage
                    .as_ref()
                    .map(|u| i64::from(u.completion_tokens)),
            },
        )?;
        for call in response_tool_calls(response) {
            add_ai_tool_call(
                &conn,
                &NewAiToolCall {
                    event_id: &self.event_id,
                    llm_tool_call_id: Some(&call.id),
                    addon_id: None,
                    tool_name: &call.function.name,
                    input_text: &call.function.arguments,
                    output_text: "",
                    // The model only REQUESTED this call here; the execution
                    // outcome lands via `record_tool_execution`. A row marked
                    // Success for a never-run tool would be a false record.
                    status: ToolCallStatus::Running,
                    error_message: None,
                    started_at: None,
                },
            )?;
        }
        let audit_log_id = insert_ai_audit_row(&conn, &self.event_id, "success", None)?;
        finish_ai_event(
            &conn,
            &self.event_id,
            AiEventStatus::Success,
            Some(audit_log_id),
            None,
        )
    }

    pub fn finish_failed(&self, error_message: &str) -> Result<()> {
        let conn = self.db.write().map_err(|_| anyhow!("blokada DB zatruta"))?;
        let audit_log_id =
            insert_ai_audit_row(&conn, &self.event_id, "error", Some(error_message))?;
        finish_ai_event(
            &conn,
            &self.event_id,
            AiEventStatus::Failed,
            Some(audit_log_id),
            Some(error_message),
        )
    }

    pub fn finish_stream_success(
        &self,
        response_text: &str,
        usage: Option<&Usage>,
        tool_calls: &[ToolCall],
    ) -> Result<()> {
        let conn = self.db.write().map_err(|_| anyhow!("blokada DB zatruta"))?;
        add_ai_payload(
            &conn,
            &NewAiPayload {
                event_id: &self.event_id,
                payload_kind: AiPayloadKind::Response,
                content_text: response_text,
                content_redacted: false,
                token_count: usage.map(|u| i64::from(u.completion_tokens)),
            },
        )?;
        for call in tool_calls {
            add_ai_tool_call(
                &conn,
                &NewAiToolCall {
                    event_id: &self.event_id,
                    llm_tool_call_id: Some(&call.id),
                    addon_id: None,
                    tool_name: &call.function.name,
                    input_text: &call.function.arguments,
                    output_text: "",
                    // Request-only row — see `finish_success`: execution
                    // outcome is recorded separately, never assumed here.
                    status: ToolCallStatus::Running,
                    error_message: None,
                    started_at: None,
                },
            )?;
        }
        let audit_log_id = insert_ai_audit_row(&conn, &self.event_id, "success", None)?;
        finish_ai_event(
            &conn,
            &self.event_id,
            AiEventStatus::Success,
            Some(audit_log_id),
            None,
        )
    }
}

fn resolve_org_id(
    conn: &Connection,
    user: Option<&UserContext>,
    context: Option<&AiGatewayContext>,
) -> Result<String> {
    if let Some(org_id) = context.and_then(|c| c.org_id.as_deref()) {
        return Ok(org_id.to_string());
    }
    if let Some(user) = user {
        let user_id = user.user_id.to_string();
        let org_id = conn
            .query_row(
                "SELECT org_id FROM org_memberships WHERE user_id = ?1 ORDER BY granted_at ASC, org_id ASC LIMIT 1",
                params![user_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("odczyt organizacji użytkownika dla AI audit")?;
        if let Some(org_id) = org_id {
            return Ok(org_id);
        }
    }
    Ok(crate::services::org::DEFAULT_ORG_ID.to_string())
}

fn chat_request_prompt_text(request: &ChatCompletionRequest) -> String {
    let mut lines = Vec::with_capacity(request.messages.len());
    for message in &request.messages {
        let content = message
            .content
            .as_ref()
            .map(message_content_text)
            .unwrap_or_default();
        lines.push(format!("{}: {}", message.role, content));
    }
    lines.join("\n")
}

fn message_content_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => text.clone(),
                ContentPart::ImageUrl { image_url } => {
                    format!("[image_url:{}]", image_url.url)
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn chat_response_text(response: &ChatCompletionResponse) -> String {
    response
        .choices
        .iter()
        .filter_map(|choice| choice.message.content.as_ref())
        .map(message_content_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn response_tool_calls(response: &ChatCompletionResponse) -> Vec<&ToolCall> {
    response
        .choices
        .iter()
        .filter_map(|choice| choice.message.tool_calls.as_ref())
        .flat_map(|calls| calls.iter())
        .collect()
}

fn insert_ai_audit_row(
    conn: &Connection,
    event_id: &str,
    result: &str,
    error_message: Option<&str>,
) -> Result<i64> {
    let event = super::repository::get_ai_event(conn, event_id)?
        .ok_or_else(|| anyhow!("brak compliance_ai_events dla {event_id}"))?;
    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let resource_id = event.event_id.as_str();
    let hash_input = crate::audit::chain::AuditRowHashInput {
        user_id: event.user_id.as_deref(),
        addon_id: event.addon_id.as_deref(),
        instance_id: event.instance_id.as_deref(),
        action: "ai.completion",
        resource: None,
        resource_type: Some("compliance_ai_event"),
        resource_id: Some(resource_id),
        result: Some(result),
        error_message,
        details: None,
        ip_address: None,
        node_id: Some(event.node_id.as_str()),
        severity: Some("info"),
        risk_class: "B",
        related_claim_id: None,
        request_id: Some(event.request_id.as_str()),
        timestamp: &timestamp,
    };
    let (prev_hash, hash) = crate::audit::chain::compute_chain_for_insert(conn, &hash_input)?;
    conn.execute(
        "INSERT INTO audit_log \
            (timestamp, user_id, addon_id, instance_id, action, resource_type, resource_id, result, error_message, severity, risk_class, request_id, org_id, node_id, prev_hash, hash) \
         VALUES (?1, ?2, ?3, ?4, 'ai.completion', 'compliance_ai_event', ?5, ?6, ?7, 'info', 'B', ?8, ?9, ?10, ?11, ?12)",
        params![
            timestamp,
            event.user_id,
            event.addon_id,
            event.instance_id,
            resource_id,
            result,
            error_message,
            event.request_id,
            event.org_id,
            event.node_id,
            prev_hash,
            hash,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::openai::types::{Choice, Message, Usage};
    use crate::db::migrations;
    use std::sync::Arc;

    fn db() -> DbPool {
        let conn = Connection::open_in_memory().expect("baza testowa");
        migrations::run(&conn).expect("migracje");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    #[test]
    fn ai_gateway_zapisuje_prompt_odpowiedz_i_audit() {
        let db = db();
        let gateway = AiGateway::new(db.clone(), "node-test");
        let request = ChatCompletionRequest {
            model: "bielik".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Cześć".to_string())),
                ..Default::default()
            }],
            temperature: None,
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            stream: false,
            stream_options: None,
            user: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            n: None,
            memory_options: None,
            audio_input: None,
        };
        let handle = gateway
            .start_chat_event(&request, None, None)
            .expect("start event");
        let response = ChatCompletionResponse {
            id: "resp-1".to_string(),
            object: "chat.completion".to_string(),
            created: 1,
            model: "bielik".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text("Dzień dobry".to_string())),
                    ..Default::default()
                },
                finish_reason: Some("stop".to_string()),
                logprobs: None,
            }],
            usage: Some(Usage {
                prompt_tokens: 3,
                completion_tokens: 4,
                total_tokens: 7,
            }),
            system_fingerprint: None,
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
            speaker_confidence: None,
            detected_intent: None,
            detected_tools: None,
        };
        handle.finish_success(&response).expect("finish event");

        let conn = db.read().expect("db lock");
        let status: String = conn
            .query_row(
                "SELECT status FROM compliance_ai_events WHERE event_id = ?1",
                params![handle.event_id()],
                |row| row.get(0),
            )
            .expect("event status");
        let payload_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM compliance_ai_payloads WHERE event_id = ?1",
                params![handle.event_id()],
                |row| row.get(0),
            )
            .expect("payload count");
        let audit_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE resource_id = ?1 AND action = 'ai.completion'",
                params![handle.event_id()],
                |row| row.get(0),
            )
            .expect("audit count");

        assert_eq!(status, "success");
        assert_eq!(payload_count, 2);
        assert_eq!(audit_count, 1);
    }

    #[test]
    fn ai_gateway_zapisuje_odpowiedz_streamingowa() {
        let db = db();
        let gateway = AiGateway::new(db.clone(), "node-test");
        let request = ChatCompletionRequest {
            model: "bielik".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Stream".to_string())),
                ..Default::default()
            }],
            temperature: None,
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            stream: true,
            stream_options: None,
            user: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            n: None,
            memory_options: None,
            audio_input: None,
        };
        let handle = gateway
            .start_chat_event(&request, None, None)
            .expect("start event");
        let usage = Usage {
            prompt_tokens: 2,
            completion_tokens: 5,
            total_tokens: 7,
        };
        handle
            .finish_stream_success("odpowiedź ze streamu", Some(&usage), &[])
            .expect("finish stream event");

        let conn = db.read().expect("db lock");
        let response_payload: String = conn
            .query_row(
                "SELECT content_text FROM compliance_ai_payloads WHERE event_id = ?1 AND payload_kind = 'response'",
                params![handle.event_id()],
                |row| row.get(0),
            )
            .expect("response payload");
        let token_count: i64 = conn
            .query_row(
                "SELECT token_count FROM compliance_ai_payloads WHERE event_id = ?1 AND payload_kind = 'response'",
                params![handle.event_id()],
                |row| row.get(0),
            )
            .expect("token count");

        assert_eq!(response_payload, "odpowiedź ze streamu");
        assert_eq!(token_count, 5);
    }

    fn sha256_hex(value: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(value.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    #[test]
    fn record_tool_execution_persists_real_results() {
        let db = db();
        let gateway = AiGateway::new(db.clone(), "node-test");
        let request = ChatCompletionRequest {
            model: "bielik".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Zapamiętaj coś".to_string())),
                ..Default::default()
            }],
            temperature: None,
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            stream: false,
            stream_options: None,
            user: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            n: None,
            memory_options: None,
            audio_input: None,
        };
        let handle = gateway
            .start_chat_event(&request, None, None)
            .expect("start event");

        let started_at = chrono::Utc::now() - chrono::Duration::seconds(2);
        let arguments = r#"{"fact":"favorite color is blue","layer":"user"}"#;
        let output = r#"{"stored":true,"memory_id":"mem-1"}"#;
        let row_id = handle
            .record_tool_execution(&ToolExecution {
                tool_call_id: "call_0_aabbccdd",
                addon_id: Some("memory"),
                tool_name: "memory.memory_store",
                arguments,
                output,
                success: true,
                error_message: None,
                started_at,
            })
            .expect("record success execution");
        handle
            .record_tool_execution(&ToolExecution {
                tool_call_id: "call_1_11223344",
                addon_id: Some("memory"),
                tool_name: "memory.memory_recall",
                arguments: r#"{"query":"color"}"#,
                output: r#"{"error":"permission denied"}"#,
                success: false,
                error_message: Some("permission denied"),
                started_at,
            })
            .expect("record failed execution");

        let conn = db.read().expect("db lock");
        let (llm_id, addon_id, input_hash, output_hash, status, row_started_at, finished_at): (
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT llm_tool_call_id, addon_id, input_hash, output_hash, status, started_at, finished_at \
                 FROM compliance_ai_tool_calls WHERE tool_call_id = ?1",
                params![row_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .expect("success row");
        assert_eq!(llm_id, "call_0_aabbccdd");
        assert_eq!(addon_id, "memory");
        assert_eq!(input_hash, sha256_hex(arguments));
        assert_eq!(output_hash, sha256_hex(output));
        assert_eq!(status, "success");
        assert_eq!(
            row_started_at,
            started_at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
        );
        let finished_at = finished_at.expect("executed call must have finished_at");
        assert!(
            finished_at.as_str() >= row_started_at.as_str(),
            "finished_at {finished_at} precedes started_at {row_started_at}"
        );

        let (failed_status, error_message): (String, Option<String>) = conn
            .query_row(
                "SELECT status, error_message FROM compliance_ai_tool_calls \
                 WHERE event_id = ?1 AND llm_tool_call_id = 'call_1_11223344'",
                params![handle.event_id()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("failed row");
        assert_eq!(failed_status, "failed");
        assert_eq!(error_message.as_deref(), Some("permission denied"));
    }

    #[test]
    fn record_run_tool_execution_attaches_to_latest_run_event() {
        let db = db();
        let gateway = AiGateway::new(db.clone(), "node-test");
        let request = ChatCompletionRequest {
            model: "bielik".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Some(MessageContent::Text("do work".to_string())),
                ..Default::default()
            }],
            temperature: None,
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            stream: false,
            stream_options: None,
            user: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            n: None,
            memory_options: None,
            audio_input: None,
        };
        // Two events for the same run; the recording must land on the second
        // (latest), which is the call the tool result answers.
        let run_ctx = AiGatewayContext {
            agent_run_id: Some("run-77".to_string()),
            ..Default::default()
        };
        let _first = gateway
            .start_chat_event(&request, None, Some(&run_ctx))
            .expect("first event");
        let second = gateway
            .start_chat_event(&request, None, Some(&run_ctx))
            .expect("second event");

        let row = gateway
            .record_run_tool_execution(
                "run-77",
                &ToolExecution {
                    tool_call_id: "call-x",
                    addon_id: None,
                    tool_name: "core.skill_view",
                    arguments: r#"{"name":"x"}"#,
                    output: r#"{"skill":"x"}"#,
                    success: true,
                    error_message: None,
                    started_at: chrono::Utc::now(),
                },
            )
            .expect("record ok");
        assert!(row.is_some(), "a run with events must record");

        // Scope the read lock so it is released before the next gateway call —
        // record_run_tool_execution re-acquires the same Mutex.
        {
            let conn = db.read().expect("db lock");
            let event_id: String = conn
                .query_row(
                    "SELECT event_id FROM compliance_ai_tool_calls WHERE llm_tool_call_id = 'call-x'",
                    [],
                    |r| r.get(0),
                )
                .expect("tool call row");
            assert_eq!(event_id, second.event_id(), "must attach to latest event");
        }

        // A run with no events records nothing (audit no-op).
        let none = gateway
            .record_run_tool_execution(
                "run-unknown",
                &ToolExecution {
                    tool_call_id: "c",
                    addon_id: None,
                    tool_name: "core.skill_view",
                    arguments: "{}",
                    output: "{}",
                    success: true,
                    error_message: None,
                    started_at: chrono::Utc::now(),
                },
            )
            .expect("record ok");
        assert!(none.is_none(), "no event for run → no row");
    }

    #[test]
    fn session_and_per_call_events_share_correlation_id() {
        let db = db();
        let gateway = AiGateway::new(db.clone(), "node-test");
        let request = ChatCompletionRequest {
            model: "bielik".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Hi".to_string())),
                ..Default::default()
            }],
            temperature: None,
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            stream: false,
            stream_options: None,
            user: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            n: None,
            memory_options: None,
            audio_input: None,
        };

        // Session/root event with no inbound correlation anchors the turn to its
        // own request_id.
        let session = gateway
            .start_chat_event(&request, None, None)
            .expect("session event");
        let correlation = session.request_id().to_string();

        // A flow per-call event carrying that correlation key links to the same
        // turn, even though it mints a distinct request_id.
        let per_call_ctx = AiGatewayContext {
            correlation_id: Some(correlation.clone()),
            ..Default::default()
        };
        let per_call = gateway
            .start_chat_event(&request, None, Some(&per_call_ctx))
            .expect("per-call event");
        assert_ne!(
            session.event_id(),
            per_call.event_id(),
            "two distinct events"
        );

        let conn = db.read().expect("db lock");
        let session_corr: String = conn
            .query_row(
                "SELECT correlation_id FROM compliance_ai_events WHERE event_id = ?1",
                params![session.event_id()],
                |r| r.get(0),
            )
            .expect("session correlation");
        let per_call_corr: String = conn
            .query_row(
                "SELECT correlation_id FROM compliance_ai_events WHERE event_id = ?1",
                params![per_call.event_id()],
                |r| r.get(0),
            )
            .expect("per-call correlation");
        assert_eq!(
            session_corr, correlation,
            "session event anchors to its own request_id"
        );
        assert_eq!(
            per_call_corr, correlation,
            "per-call event copies the session correlation key"
        );
    }
}
