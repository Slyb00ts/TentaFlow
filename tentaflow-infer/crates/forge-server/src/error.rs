// ===== File: error.rs — OpenAI-shaped API errors with HTTP status mapping =====

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

/// One API failure; renders as the OpenAI `{"error": {...}}` envelope.
#[derive(Debug, Clone, Serialize)]
pub struct ApiErrorBody {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: ApiErrorBody,
    /// Seconds hint for 429 responses (`Retry-After` header).
    pub retry_after: Option<u32>,
}

impl ApiError {
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: ApiErrorBody {
                message: message.into(),
                error_type: "invalid_request_error".into(),
                code: None,
            },
            retry_after: None,
        }
    }

    pub fn model_not_found(requested: &str, served: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            body: ApiErrorBody {
                message: format!("model {requested:?} not found; this server serves {served:?}"),
                error_type: "invalid_request_error".into(),
                code: Some("model_not_found".into()),
            },
            retry_after: None,
        }
    }

    pub fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            body: ApiErrorBody {
                message: "missing or invalid API key".into(),
                error_type: "invalid_request_error".into(),
                code: Some("invalid_api_key".into()),
            },
            retry_after: None,
        }
    }

    pub fn context_length_exceeded(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: ApiErrorBody {
                message: message.into(),
                error_type: "invalid_request_error".into(),
                code: Some("context_length_exceeded".into()),
            },
            retry_after: None,
        }
    }

    pub fn overloaded(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            body: ApiErrorBody {
                message: message.into(),
                error_type: "rate_limit_error".into(),
                code: Some("engine_overloaded".into()),
            },
            retry_after: Some(1),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: ApiErrorBody {
                message: message.into(),
                error_type: "server_error".into(),
                code: None,
            },
            retry_after: None,
        }
    }

    /// Map an engine-side error string to a transport status. "cache has N
    /// total" / size overflow mean the request can NEVER fit (permanent →
    /// 400); any other KV-page message is transient pressure (429).
    pub fn from_engine_error(message: &str) -> Self {
        if message.contains("KV pages, cache has") || message.contains("request size overflows") {
            Self::context_length_exceeded(message.to_string())
        } else if message.contains("KV pages") {
            Self::overloaded(message.to_string())
        } else {
            Self::internal(message.to_string())
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut resp =
            (self.status, Json(serde_json::json!({ "error": self.body }))).into_response();
        if let Some(secs) = self.retry_after {
            resp.headers_mut().insert(header::RETRY_AFTER, secs.into());
        }
        resp
    }
}
