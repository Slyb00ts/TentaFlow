// =============================================================================
// Plik: intent_analyzer/executor.rs
// Opis: Executor wywolan narzedzi — zaślepki logujace do konsoli.
//       Docelowo beda wywoływać prawdziwe API (Google Calendar, SMTP, etc.).
// =============================================================================

//! Tool Executor - wykonuje wywołania narzędzi
//!
//! Na razie to są ZAŚLEPKI które tylko logują do konsoli.
//! Docelowo będą wywoływać prawdziwe API (Google Calendar, SMTP, etc.)

use super::types::*;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Wynik wykonania narzędzia
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    /// ID wywołania (do korelacji)
    pub call_id: String,
    /// Czy wykonanie się powiodło
    pub success: bool,
    /// Wiadomość zwrotna (dla użytkownika)
    pub message: String,
    /// Dane zwrotne (opcjonalne, zależne od narzędzia)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Błąd (jeśli success=false)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Executor narzędzi - wykonuje tool calls
pub struct ToolExecutor;

impl ToolExecutor {
    /// Wykonuje pojedyncze wywołanie narzędzia
    ///
    /// UWAGA: Na razie to są ZAŚLEPKI - tylko logują do konsoli!
    pub async fn execute(tool_result: &ToolCallResult) -> ToolExecutionResult {
        if !tool_result.is_complete {
            return ToolExecutionResult {
                call_id: tool_result.call_id.clone(),
                success: false,
                message: tool_result
                    .follow_up_question
                    .clone()
                    .unwrap_or_else(|| "Brakujące parametry".to_string()),
                data: None,
                error: Some(format!(
                    "Missing required parameters: {:?}",
                    tool_result.missing_params
                )),
            };
        }

        match &tool_result.tool {
            ToolCall::CalendarAdd(params) => {
                Self::execute_calendar_add(&tool_result.call_id, params).await
            }
            ToolCall::CalendarCheck(params) => {
                Self::execute_calendar_check(&tool_result.call_id, params).await
            }
            ToolCall::EmailSend(params) => {
                Self::execute_email_send(&tool_result.call_id, params).await
            }
            ToolCall::WebSearch(params) => {
                Self::execute_web_search(&tool_result.call_id, params).await
            }
            ToolCall::ReminderSet(params) => {
                Self::execute_reminder_set(&tool_result.call_id, params).await
            }
            ToolCall::TimerSet(params) => {
                Self::execute_timer_set(&tool_result.call_id, params).await
            }
            ToolCall::NoteSave(params) => {
                Self::execute_note_save(&tool_result.call_id, params).await
            }
        }
    }

    /// Wykonuje listę wywołań narzędzi równolegle
    pub async fn execute_all(tool_results: &[ToolCallResult]) -> Vec<ToolExecutionResult> {
        let futures: Vec<_> = tool_results.iter().map(|tr| Self::execute(tr)).collect();
        futures::future::join_all(futures).await
    }

    // ========================================================================
    // ZAŚLEPKI - tylko logują do konsoli
    // ========================================================================

    async fn execute_calendar_add(
        call_id: &str,
        params: &CalendarAddParams,
    ) -> ToolExecutionResult {
        info!(call_id = %call_id, tool = "CALENDAR_ADD", "(STUB)");
        debug!("title: {:?}, date: {:?}, start_time: {:?}, end_time: {:?}, duration: {:?}, location: {:?}, attendees: {:?}",
            params.title, params.date, params.start_time, params.end_time,
            params.duration, params.location, params.attendees);

        // Symuluj sukces
        let title = params.title.as_deref().unwrap_or("wydarzenie");
        let date = params.date.as_deref().unwrap_or("?");

        ToolExecutionResult {
            call_id: call_id.to_string(),
            success: true,
            message: format!("Dodałem \"{}\" do kalendarza na {}.", title, date),
            data: Some(serde_json::json!({
                "event_id": format!("evt_{}", &uuid::Uuid::new_v4().to_string()[..8]),
                "title": params.title,
                "date": params.date,
                "status": "created"
            })),
            error: None,
        }
    }

    async fn execute_calendar_check(
        call_id: &str,
        params: &CalendarCheckParams,
    ) -> ToolExecutionResult {
        info!(call_id = %call_id, tool = "CALENDAR_CHECK", "(STUB)");
        debug!(
            "date: {:?}, date_range: {:?}, search_query: {:?}",
            params.date, params.date_range, params.search_query
        );

        let date = params
            .date
            .as_deref()
            .or(params.date_range.as_deref())
            .unwrap_or("dziś");

        // Symuluj pustą odpowiedź (brak wydarzeń)
        ToolExecutionResult {
            call_id: call_id.to_string(),
            success: true,
            message: format!("Na {} nie masz żadnych zaplanowanych wydarzeń.", date),
            data: Some(serde_json::json!({
                "events": [],
                "date_checked": date
            })),
            error: None,
        }
    }

    async fn execute_email_send(call_id: &str, params: &EmailSendParams) -> ToolExecutionResult {
        info!(call_id = %call_id, tool = "EMAIL_SEND", "(STUB)");
        debug!(
            "to: {:?}, subject: {:?}, body_len: {}, cc: {:?}, priority: {:?}",
            params.to,
            params.subject,
            params.body.as_ref().map_or(0, |b| b.len()),
            params.cc,
            params.priority
        );

        let to = params.to.as_deref().unwrap_or("?");
        let subject = params.subject.as_deref().unwrap_or("(bez tematu)");

        ToolExecutionResult {
            call_id: call_id.to_string(),
            success: true,
            message: format!("Wysłałem email do {} z tematem \"{}\".", to, subject),
            data: Some(serde_json::json!({
                "message_id": format!("msg_{}", &uuid::Uuid::new_v4().to_string()[..8]),
                "to": params.to,
                "subject": params.subject,
                "status": "sent"
            })),
            error: None,
        }
    }

    async fn execute_web_search(call_id: &str, params: &WebSearchParams) -> ToolExecutionResult {
        info!(call_id = %call_id, tool = "WEB_SEARCH", "(STUB)");
        debug!(
            "query: {:?}, search_type: {:?}, language: {:?}, max_results: {:?}",
            params.query, params.search_type, params.language, params.max_results
        );

        let query = params.query.as_deref().unwrap_or("?");

        // Symuluj wyniki wyszukiwania
        ToolExecutionResult {
            call_id: call_id.to_string(),
            success: true,
            message: format!("Znalazłem wyniki dla \"{}\".", query),
            data: Some(serde_json::json!({
                "query": query,
                "results": [
                    {
                        "title": "Przykładowy wynik 1",
                        "url": "https://example.com/1",
                        "snippet": "To jest przykładowy wynik wyszukiwania..."
                    },
                    {
                        "title": "Przykładowy wynik 2",
                        "url": "https://example.com/2",
                        "snippet": "Kolejny przykładowy wynik..."
                    }
                ],
                "total_results": 2
            })),
            error: None,
        }
    }

    async fn execute_reminder_set(
        call_id: &str,
        params: &ReminderSetParams,
    ) -> ToolExecutionResult {
        info!(call_id = %call_id, tool = "REMINDER_SET", "(STUB)");
        debug!(
            "message: {:?}, when: {:?}, repeat: {:?}",
            params.message, params.when, params.repeat
        );

        let message = params.message.as_deref().unwrap_or("przypomnienie");
        let when = params.when.as_deref().unwrap_or("?");

        ToolExecutionResult {
            call_id: call_id.to_string(),
            success: true,
            message: format!("Ustawiłem przypomnienie \"{}\" na {}.", message, when),
            data: Some(serde_json::json!({
                "reminder_id": format!("rem_{}", &uuid::Uuid::new_v4().to_string()[..8]),
                "message": params.message,
                "when": params.when,
                "status": "scheduled"
            })),
            error: None,
        }
    }

    async fn execute_timer_set(call_id: &str, params: &TimerSetParams) -> ToolExecutionResult {
        info!(call_id = %call_id, tool = "TIMER_SET", "(STUB)");
        debug!("duration: {:?}, label: {:?}", params.duration, params.label);

        let duration = params.duration.as_deref().unwrap_or("?");
        let label = params.label.as_deref().unwrap_or("Timer");

        ToolExecutionResult {
            call_id: call_id.to_string(),
            success: true,
            message: format!("Ustawiłem timer \"{}\" na {}.", label, duration),
            data: Some(serde_json::json!({
                "timer_id": format!("tmr_{}", &uuid::Uuid::new_v4().to_string()[..8]),
                "duration": params.duration,
                "label": params.label,
                "status": "running"
            })),
            error: None,
        }
    }

    async fn execute_note_save(call_id: &str, params: &NoteSaveParams) -> ToolExecutionResult {
        info!(call_id = %call_id, tool = "NOTE_SAVE", "(STUB)");
        debug!(
            "title: {:?}, content_len: {}, tags: {:?}",
            params.title,
            params.content.as_ref().map_or(0, |c| c.len()),
            params.tags
        );

        let title = params.title.as_deref().unwrap_or("Notatka");

        ToolExecutionResult {
            call_id: call_id.to_string(),
            success: true,
            message: format!("Zapisałem notatkę \"{}\".", title),
            data: Some(serde_json::json!({
                "note_id": format!("note_{}", &uuid::Uuid::new_v4().to_string()[..8]),
                "title": params.title,
                "status": "saved"
            })),
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_calendar_add_stub() {
        let tool = ToolCall::CalendarAdd(CalendarAddParams {
            title: Some("Spotkanie".to_string()),
            date: Some("2024-01-15".to_string()),
            ..Default::default()
        });

        let tool_result = ToolCallResult::new(tool);
        assert!(tool_result.is_complete);

        let result = ToolExecutor::execute(&tool_result).await;
        assert!(result.success);
        assert!(result.message.contains("Spotkanie"));
    }

    #[tokio::test]
    async fn test_incomplete_tool_call() {
        let tool = ToolCall::CalendarAdd(CalendarAddParams {
            title: Some("Spotkanie".to_string()),
            date: None, // Brakuje daty!
            ..Default::default()
        });

        let tool_result = ToolCallResult::new(tool);
        assert!(!tool_result.is_complete);
        assert_eq!(tool_result.missing_params, vec!["date"]);

        let result = ToolExecutor::execute(&tool_result).await;
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn test_web_search_stub() {
        let tool = ToolCall::WebSearch(WebSearchParams {
            query: Some("rust programming".to_string()),
            ..Default::default()
        });

        let tool_result = ToolCallResult::new(tool);
        assert!(tool_result.is_complete);

        let result = ToolExecutor::execute(&tool_result).await;
        assert!(result.success);
        assert!(result.data.is_some());
    }
}
