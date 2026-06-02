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
        let conn = self.db.lock().map_err(|_| anyhow!("blokada DB zatruta"))?;
        let org_id = resolve_org_id(&conn, user, context)?;
        let request_id = uuid::Uuid::new_v4().to_string();
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
                request_id: &request_id,
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
        })
    }
}

impl AiEventHandle {
    pub fn event_id(&self) -> &str {
        &self.event_id
    }

    pub fn finish_success(&self, response: &ChatCompletionResponse) -> Result<()> {
        let conn = self.db.lock().map_err(|_| anyhow!("blokada DB zatruta"))?;
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
                    addon_id: None,
                    tool_name: &call.function.name,
                    input_text: &call.function.arguments,
                    output_text: "",
                    status: ToolCallStatus::Success,
                    error_message: None,
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
        let conn = self.db.lock().map_err(|_| anyhow!("blokada DB zatruta"))?;
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
        let conn = self.db.lock().map_err(|_| anyhow!("blokada DB zatruta"))?;
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
                    addon_id: None,
                    tool_name: &call.function.name,
                    input_text: &call.function.arguments,
                    output_text: "",
                    status: ToolCallStatus::Success,
                    error_message: None,
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
    use std::sync::{Arc, Mutex};

    fn db() -> DbPool {
        let conn = Connection::open_in_memory().expect("baza testowa");
        migrations::run(&conn).expect("migracje");
        Arc::new(Mutex::new(conn))
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

        let conn = db.lock().expect("db lock");
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

        let conn = db.lock().expect("db lock");
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
}
