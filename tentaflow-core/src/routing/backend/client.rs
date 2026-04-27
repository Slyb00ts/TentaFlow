// =============================================================================
// Plik: routing/backend/client.rs
// Opis: HTTP client dla komunikacji z backendami LLM (OpenAI, Azure OpenAI,
//       NVIDIA NIM, lokalne modele). Obsluguje chat completions (streaming
//       i non-streaming), embeddings, audio transcription (Whisper), vision.
// =============================================================================

use crate::config::ServiceBackend;
use crate::error::{CoreError, Result};
use crate::routing::loadbalancer::{CircuitBreaker, CircuitBreakerConfig};

// TODO: typy OpenAI API nie sa jeszcze przeniesione do Core
// Po przeniesieniu modulu protocols::openai::types zamien na:
//   use crate::api::openai::types::*;
// Tymczasowo importujemy z Router crate (wymaga dodania zaleznosci)
// lub nalezy najpierw przeniesc typy OpenAI do crate::api::openai::types
use crate::api::openai::types::*;

use reqwest::{Client, StatusCode};
use std::time::Duration;
use tracing::{debug, error, warn};

// Dla streaming support
use futures::stream::{Stream, StreamExt};
use std::pin::Pin;

/// HTTP Client dla pojedynczego backendu.
///
/// Uzywa reqwest::Client z connection pooling i timeout.
/// Kazdy backend ma wlasna instancje klienta z osobna konfiguracja.
pub struct BackendClient {
    /// Konfiguracja backendu
    config: ServiceBackend,

    /// HTTP client (reqwest) - reusable, z connection pooling
    client: Client,

    /// API key dla tego backendu (wyekstraktowany z ConnectionType)
    #[allow(dead_code)]
    api_key: String,

    /// Pre-built Authorization header (unika format!() w kazdym requescie)
    auth_header_value: String,

    /// URL backendu (wyekstraktowany z ConnectionType dla szybkiego dostepu)
    url: String,

    /// Custom endpoint path (opcjonalny, wyekstraktowany z ConnectionType)
    #[allow(dead_code)]
    custom_endpoint: Option<String>,

    /// Request format transformation (wyekstraktowany z ConnectionType)
    request_format: Option<String>,

    /// Circuit breaker dla ochrony przed awariami
    circuit_breaker: CircuitBreaker,

    /// Pre-built URL dla /chat/completions (lub custom_endpoint)
    chat_completions_url: String,

    /// Pre-built URL dla /embeddings
    embeddings_url: String,

    /// Pre-built URL dla /audio/transcriptions
    audio_transcriptions_url: String,
}

impl BackendClient {
    /// Tworzy nowy backend client.
    ///
    /// Ekstraktuje pola z ConnectionType::OpenAIApi i konfiguruje HTTP client.
    /// Wczytuje API key z config lub zmiennej srodowiskowej.
    ///
    /// Parametry:
    /// - config: ServiceBackend z ConnectionType::OpenAIApi
    /// - circuit_breaker_config: Konfiguracja circuit breakera (opcjonalna, domyslna jesli None)
    ///
    /// Zwraca: Instancje BackendClient
    /// Bledy: ConfigError jesli ConnectionType nie jest OpenAIApi lub API key nie jest ustawiony
    pub fn new(
        config: ServiceBackend,
        circuit_breaker_config: Option<CircuitBreakerConfig>,
    ) -> Result<Self> {
        use crate::config::ConnectionType;

        // Ekstraktuj pola z ConnectionType::OpenAIApi
        let (url, api_key_opt, api_key_env_opt, custom_endpoint, request_format) =
            match &config.connection {
                ConnectionType::OpenAIApi {
                    url,
                    api_key,
                    api_key_env,
                    custom_endpoint,
                    request_format,
                    ..
                } => (
                    url.clone(),
                    api_key.clone(),
                    api_key_env.clone(),
                    custom_endpoint.clone(),
                    request_format.clone(),
                ),
                ConnectionType::QUIC { .. } => {
                    return Err(CoreError::ConfigError {
                        message: "BackendClient wymaga ConnectionType::OpenAIApi, otrzymano QUIC"
                            .to_string(),
                        source: anyhow::anyhow!("Invalid connection type for BackendClient"),
                    }
                    .into());
                }
            };

        // Wczytaj API key: priorytet dla direct key, fallback do env var
        let api_key = if let Some(key) = api_key_opt {
            key
        } else if let Some(env_var) = api_key_env_opt {
            std::env::var(&env_var).map_err(|_| CoreError::ConfigError {
                message: format!("Zmienna srodowiskowa '{}' nie jest ustawiona", env_var),
                source: anyhow::anyhow!("Missing API key env var"),
            })?
        } else {
            return Err(CoreError::ConfigError {
                message: "Brak api_key ani api_key_env w konfiguracji backend".to_string(),
                source: anyhow::anyhow!("No API key configured"),
            }
            .into());
        };

        // Utworz reqwest::Client z timeout
        let client = Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .pool_max_idle_per_host(config.max_concurrent) // Connection pooling
            .build()
            .map_err(|e| CoreError::InternalError {
                message: "Nie mozna utworzyc HTTP client".to_string(),
                source: Some(e.into()),
            })?;

        // Utworz circuit breaker z konfiguracja
        let cb_config = circuit_breaker_config.unwrap_or_else(|| CircuitBreakerConfig::default());
        let backend_name = url.clone();
        let circuit_breaker = CircuitBreaker::new(backend_name, cb_config);

        let auth_header_value = format!("Bearer {}", api_key);

        // Pre-build URL-e dla poszczegolnych endpointow
        let base = url.trim_end_matches('/');
        let chat_completions_url = format!(
            "{}{}",
            base,
            custom_endpoint.as_deref().unwrap_or("/chat/completions")
        );
        let embeddings_url = format!("{}/embeddings", base);
        let audio_transcriptions_url = format!("{}/audio/transcriptions", base);

        debug!(
            "Backend client utworzony dla: {} (timeout: {}ms, circuit breaker: enabled)",
            url, config.timeout_ms
        );

        Ok(Self {
            config,
            url,
            custom_endpoint,
            request_format,
            client,
            api_key,
            auth_header_value,
            circuit_breaker,
            chat_completions_url,
            embeddings_url,
            audio_transcriptions_url,
        })
    }

    /// Sprawdza circuit breaker - zwraca blad jesli backend niedostepny (OPEN)
    fn check_circuit_breaker(&self) -> Result<()> {
        if !self.circuit_breaker.can_execute() {
            return Err(CoreError::BackendError {
                backend_url: self.url.clone(),
                message: "Circuit breaker OPEN - backend unavailable".to_string(),
                source: None,
            }
            .into());
        }
        Ok(())
    }

    /// Zastosuj model_name_override na podanym modelu jesli skonfigurowano
    fn apply_model_override(&self, model: &mut String) {
        if let Some(override_name) = &self.config.model_name_override {
            debug!("Model override: {} -> {}", model, override_name);
            *model = override_name.clone();
        }
    }

    /// Wysyla chat completion request do backendu.
    ///
    /// Parsuje request do JSON, wysyla POST do /v1/chat/completions,
    /// i zwraca sparsowana odpowiedz.
    pub async fn chat_completion(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        self.check_circuit_breaker()?;
        self.apply_model_override(&mut request.model);

        let url = &self.chat_completions_url;
        debug!("Wysylanie chat completion do: {}", url);

        // Wyslij POST request (dla formatu openai bezposrednia serializacja, inaczej transformacja)
        let response = if self.needs_transform() {
            let request_body = self.transform_request(&request)?;
            self.client
                .post(url)
                .header("Authorization", self.auth_header_value.as_str())
                .header("Content-Type", "application/json")
                .json(&request_body)
                .send()
                .await
        } else {
            self.client
                .post(url)
                .header("Authorization", self.auth_header_value.as_str())
                .header("Content-Type", "application/json")
                .json(&request)
                .send()
                .await
        }
        .map_err(|e| {
            let error = self.map_reqwest_error(e);
            self.circuit_breaker.record_failure();
            error
        })?;

        let status = response.status();
        debug!("Response status: {}", status);

        // Sprawdz status code
        if !status.is_success() {
            // 4xx (client errors) NIE powinny triggerowac circuit breakera
            if status.is_server_error() {
                self.circuit_breaker.record_failure();
            }
            return self.handle_error_response(status, response).await;
        }

        // Parsuj JSON response
        // Jesli backend wymaga transformacji odpowiedzi (np. PaddleOCR), parsuj jako Value i transformuj
        let completion = if self.request_format.as_deref() == Some("paddleocr") {
            let response_value = response.json::<serde_json::Value>().await.map_err(|e| {
                CoreError::BackendError {
                    backend_url: self.url.clone(),
                    message: format!("Nie mozna sparsowac odpowiedzi: {}", e),
                    source: Some(e.into()),
                }
            })?;

            self.transform_response(&response_value)?
        } else {
            response
                .json::<ChatCompletionResponse>()
                .await
                .map_err(|e| CoreError::BackendError {
                    backend_url: self.url.clone(),
                    message: format!("Nie mozna sparsowac odpowiedzi: {}", e),
                    source: Some(e.into()),
                })?
        };

        debug!(
            "Chat completion OK: {} tokens",
            completion
                .usage
                .as_ref()
                .map(|u| u.total_tokens)
                .unwrap_or(0)
        );

        self.circuit_breaker.record_success();

        Ok(completion)
    }

    /// Wysyla streaming chat completion request do backendu.
    ///
    /// Zwraca Stream chunkow SSE zamiast pojedynczej odpowiedzi.
    pub async fn chat_completion_stream(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>> {
        self.check_circuit_breaker()?;

        // Wymuszamy stream = true
        request.stream = true;

        self.apply_model_override(&mut request.model);

        let url = &self.chat_completions_url;
        debug!("Wysylanie streaming chat completion do: {}", url);

        // Wyslij POST request
        let response = self
            .client
            .post(url)
            .header("Authorization", self.auth_header_value.as_str())
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                let error = self.map_reqwest_error(e);
                self.circuit_breaker.record_failure();
                error
            })?;

        let status = response.status();
        debug!("Streaming response status: {}", status);

        // Sprawdz status code
        if !status.is_success() {
            if status.is_server_error() {
                self.circuit_breaker.record_failure();
            }
            // Dla streaming error response musimy przeczytac cale body
            let error_body = response.text().await.unwrap_or_else(|_| String::new());
            return Err(CoreError::BackendError {
                backend_url: self.url.clone(),
                message: format!("Backend zwrocil blad HTTP {}: {}", status, error_body),
                source: None,
            }
            .into());
        }

        // Konwertuj response na byte stream
        let byte_stream = response.bytes_stream();

        // Przetwarz byte stream na SSE chunks
        let backend_url = self.url.clone();
        let stream = byte_stream
            .map(move |chunk_result| {
                chunk_result.map_err(|e| CoreError::NetworkError {
                    message: format!("Blad czytania streamu: {}", e),
                    source: e.into(),
                })
            })
            .scan(String::new(), move |buffer, chunk_result| {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => return futures::future::ready(Some(vec![Err(e)])),
                };

                // Dodaj nowe bajty do bufora (unika alokacji Cow::Owned gdy UTF-8 poprawne)
                match std::str::from_utf8(&chunk) {
                    Ok(s) => buffer.push_str(s),
                    Err(_) => buffer.push_str(&String::from_utf8_lossy(&chunk)),
                }

                // Parsuj linie zakonczone \n
                let mut results = Vec::new();
                while let Some(newline_pos) = buffer.find('\n') {
                    let line: String = buffer[..newline_pos].into();
                    buffer.drain(..=newline_pos);

                    // Pomin puste linie
                    if line.trim().is_empty() {
                        continue;
                    }

                    // Parsuj SSE line: "data: {...}" lub "data: [DONE]"
                    if let Some(json_str) = line.strip_prefix("data: ") {
                        // Sprawdz czy to [DONE]
                        if json_str.trim() == "[DONE]" {
                            debug!("Stream zakonczony [DONE]");
                            continue;
                        }

                        // Parsuj JSON do ChatCompletionChunk
                        debug!("Raw SSE JSON: {}", json_str);
                        match serde_json::from_str::<ChatCompletionChunk>(json_str) {
                            Ok(chunk) => {
                                debug!(
                                    "Parsed chunk: id={}, choices={} deltas",
                                    chunk.id,
                                    chunk.choices.len()
                                );
                                if let Some(first_choice) = chunk.choices.first() {
                                    debug!(
                                        "   Delta content: {:?}, reasoning: {:?}",
                                        first_choice.delta.content,
                                        first_choice.delta.reasoning_content
                                    );
                                }
                                results.push(Ok(chunk))
                            }
                            Err(e) => {
                                warn!("Nie mozna sparsowac chunk: {} - blad: {}", json_str, e);
                                results.push(Err(CoreError::BackendError {
                                    backend_url: backend_url.clone(),
                                    message: format!("Nieprawidlowy JSON w SSE chunk: {}", e),
                                    source: Some(e.into()),
                                }));
                            }
                        }
                    }
                }

                // Zawsze zwracaj Some - None konczy stream!
                // Pusta lista zostanie odfiltrowana przez flat_map
                futures::future::ready(Some(results))
            })
            .flat_map(futures::stream::iter)
            .map(|result| result.map_err(|e| e.into())); // Konwertuj CoreError -> anyhow::Error

        Ok(Box::pin(stream))
    }

    /// Mapuje reqwest::Error na CoreError.
    ///
    /// Rozroznia typy bledow (timeout, network, etc.) i zwraca odpowiedni CoreError.
    fn map_reqwest_error(&self, err: reqwest::Error) -> CoreError {
        if err.is_timeout() {
            CoreError::Timeout {
                backend_url: self.url.clone(),
                timeout_ms: self.config.timeout_ms,
            }
        } else if err.is_connect() || err.is_request() {
            CoreError::NetworkError {
                message: format!("Blad polaczenia z backendem: {}", self.url),
                source: err.into(),
            }
        } else {
            CoreError::BackendError {
                backend_url: self.url.clone(),
                message: format!("Blad reqwest: {}", err),
                source: Some(err.into()),
            }
        }
    }

    /// Obsluguje error response z backendu (HTTP 4xx/5xx).
    ///
    /// Probuje sparsowac ErrorResponse z body, jesli sie nie uda zwraca ogolny blad.
    async fn handle_error_response(
        &self,
        status: StatusCode,
        response: reqwest::Response,
    ) -> Result<ChatCompletionResponse> {
        // Probuj sparsowac error body
        let error_body = response.text().await.unwrap_or_else(|_| String::new());

        // Probuj sparsowac jako ErrorResponse
        let error_message =
            if let Ok(error_response) = serde_json::from_str::<ErrorResponse>(&error_body) {
                format!(
                    "Backend error: {} ({})",
                    error_response.error.message, error_response.error.error_type
                )
            } else {
                format!(
                    "Backend zwrocil blad HTTP {}: {}",
                    status,
                    error_body.chars().take(200).collect::<String>()
                )
            };

        error!("{}", error_message);

        Err(CoreError::BackendError {
            backend_url: self.url.clone(),
            message: error_message,
            source: None,
        }
        .into())
    }

    /// Wysyla embedding request do backendu.
    ///
    /// Konwertuje Vec<String> do EmbeddingRequest, wysyla POST do /v1/embeddings,
    /// i zwraca wektory embedding.
    pub async fn embedding(&self, input: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.check_circuit_breaker()?;

        let url = &self.embeddings_url;
        debug!(
            "Wysylanie embedding request do: {} ({} tekstow)",
            url,
            input.len()
        );

        // Utworz embedding request
        let request = EmbeddingRequest {
            model: self
                .config
                .model_name_override
                .clone()
                .unwrap_or_else(|| "text-embedding-3-small".to_string()),
            input: EmbeddingInput::Multiple(input),
            encoding_format: Some("float".to_string()),
            dimensions: None,
            user: None,
        };

        // Wyslij POST request
        let response = self
            .client
            .post(url)
            .header("Authorization", self.auth_header_value.as_str())
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                let error = self.map_reqwest_error(e);
                self.circuit_breaker.record_failure();
                error
            })?;

        let status = response.status();
        debug!("Embedding response status: {}", status);

        // Sprawdz status code
        if !status.is_success() {
            if status.is_server_error() {
                self.circuit_breaker.record_failure();
            }
            let error_body = response.text().await.unwrap_or_else(|_| String::new());
            return Err(CoreError::BackendError {
                backend_url: self.url.clone(),
                message: format!("Embedding API error ({}): {}", status, error_body),
                source: None,
            }
            .into());
        }

        // Parsuj JSON response
        let embedding_response =
            response
                .json::<EmbeddingResponse>()
                .await
                .map_err(|e| CoreError::BackendError {
                    backend_url: self.url.clone(),
                    message: format!("Nie mozna sparsowac embedding response: {}", e),
                    source: Some(e.into()),
                })?;

        // Sortuj embeddingi po index (na wypadek gdyby byly w innej kolejnosci)
        let mut data = embedding_response.data;
        data.sort_by_key(|d| d.index);

        // Wyciagnij wektory embedding
        let embeddings: Vec<Vec<f32>> = data.into_iter().map(|d| d.embedding).collect();

        debug!("Otrzymano {} embeddingow", embeddings.len());

        self.circuit_breaker.record_success();

        Ok(embeddings)
    }

    /// Wysyla embeddings request do backendu (pelna wersja z EmbeddingRequest).
    ///
    /// Wysyla POST do /v1/embeddings z pelnym EmbeddingRequest i zwraca
    /// pelny EmbeddingResponse (zgodny z OpenAI API).
    pub async fn embeddings_request(
        &self,
        mut request: EmbeddingRequest,
    ) -> Result<EmbeddingResponse> {
        self.check_circuit_breaker()?;
        self.apply_model_override(&mut request.model);

        let url = &self.embeddings_url;

        debug!(
            "Wysylanie embeddings request do: {} (model: {})",
            url, request.model
        );

        // Wyslij POST request
        let response = self
            .client
            .post(url)
            .header("Authorization", self.auth_header_value.as_str())
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                let error = self.map_reqwest_error(e);
                self.circuit_breaker.record_failure();
                error
            })?;

        let status = response.status();
        debug!("Embeddings response status: {}", status);

        // Sprawdz status code
        if !status.is_success() {
            if status.is_server_error() {
                self.circuit_breaker.record_failure();
            }
            let error_body = response.text().await.unwrap_or_else(|_| String::new());
            return Err(CoreError::BackendError {
                backend_url: self.url.clone(),
                message: format!("Embeddings API error ({}): {}", status, error_body),
                source: None,
            }
            .into());
        }

        // Parsuj JSON response
        let embedding_response =
            response
                .json::<EmbeddingResponse>()
                .await
                .map_err(|e| CoreError::BackendError {
                    backend_url: self.url.clone(),
                    message: format!("Nie mozna sparsowac embeddings response: {}", e),
                    source: Some(e.into()),
                })?;

        debug!(
            "Otrzymano embeddings response: {} embeddingow",
            embedding_response.data.len()
        );

        self.circuit_breaker.record_success();

        Ok(embedding_response)
    }

    /// Wysyla audio transcription request do backendu (Whisper).
    ///
    /// Tworzy multipart/form-data request z plikiem audio i parametrami,
    /// wysyla POST do /v1/audio/transcriptions i zwraca transkrypcje.
    pub async fn audio_transcription(
        &self,
        mut request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse> {
        self.check_circuit_breaker()?;
        self.apply_model_override(&mut request.model);

        let url = &self.audio_transcriptions_url;

        debug!(
            "Wysylanie audio transcription request do: {} (plik: {}, rozmiar: {} bajtow)",
            url,
            request.filename,
            request.file.len()
        );

        // Utworz multipart/form-data request
        let file_part = reqwest::multipart::Part::bytes(request.file.as_ref().to_vec())
            .file_name(request.filename.clone())
            .mime_str("audio/mpeg")
            .map_err(|e| CoreError::InternalError {
                message: "Nie mozna utworzyc file part".to_string(),
                source: Some(e.into()),
            })?;

        let mut form = reqwest::multipart::Form::new()
            .part("file", file_part)
            .text("model", request.model.clone());

        // Dodaj opcjonalne parametry
        debug!(
            "Audio transcription params: model={}, language={:?}",
            request.model, request.language
        );
        if let Some(language) = request.language {
            debug!("Adding language to form: {}", language);
            form = form.text("language", language);
        }
        if let Some(prompt) = request.prompt {
            form = form.text("prompt", prompt);
        }
        if let Some(response_format) = request.response_format {
            form = form.text("response_format", response_format);
        }
        if let Some(temperature) = request.temperature {
            form = form.text("temperature", temperature.to_string());
        }
        // timestamp_granularities - tylko dla verbose_json
        // Whisper API przyjmuje "timestamp_granularities[]" jako array
        if let Some(granularities) = &request.timestamp_granularities {
            for g in granularities {
                form = form.text("timestamp_granularities[]", g.clone());
            }
        }

        // Wyslij POST request
        let response = self
            .client
            .post(url)
            .header("Authorization", self.auth_header_value.as_str())
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                let error = self.map_reqwest_error(e);
                self.circuit_breaker.record_failure();
                error
            })?;

        let status = response.status();
        debug!("Audio transcription response status: {}", status);

        // Sprawdz status code
        if !status.is_success() {
            if status.is_server_error() {
                self.circuit_breaker.record_failure();
            }
            let error_body = response.text().await.unwrap_or_else(|_| String::new());
            return Err(CoreError::BackendError {
                backend_url: self.url.clone(),
                message: format!("Audio transcription API error ({}): {}", status, error_body),
                source: None,
            }
            .into());
        }

        // Parsuj JSON response
        let transcription_response =
            response
                .json::<TranscriptionResponse>()
                .await
                .map_err(|e| CoreError::BackendError {
                    backend_url: self.url.clone(),
                    message: format!("Nie mozna sparsowac audio transcription response: {}", e),
                    source: Some(e.into()),
                })?;

        debug!(
            "Audio transcription zakonczona: {} znakow",
            transcription_response.text.len()
        );

        self.circuit_breaker.record_success();

        Ok(transcription_response)
    }

    /// Wysyla vision request do backendu (image understanding/OCR).
    ///
    /// Konwertuje VisionMessage do formatu chat completions i wysyla do backendu.
    /// Vision API jest implementowane jako chat completions z obrazami w tresci.
    pub async fn vision(
        &self,
        mut model: String,
        messages: Vec<tentaflow_protocol::VisionMessage>,
        max_tokens: Option<u32>,
    ) -> Result<String> {
        use crate::api::openai::types::{ContentPart, ImageUrl, Message, MessageContent};

        self.check_circuit_breaker()?;
        self.apply_model_override(&mut model);

        let url = &self.chat_completions_url;

        debug!(
            "Wysylanie vision request do: {} (model: {}, messages: {})",
            url,
            model,
            messages.len()
        );

        // Konwertuj VisionMessage -> ChatCompletionRequest Message
        let chat_messages: Vec<Message> = messages
            .into_iter()
            .map(|vm| {
                let content_parts: Vec<ContentPart> = vm
                    .content
                    .into_iter()
                    .map(|part| match part {
                        tentaflow_protocol::VisionContentPart::Text { text } => {
                            ContentPart::Text { text }
                        }
                        tentaflow_protocol::VisionContentPart::ImageUrl { url, detail } => {
                            ContentPart::ImageUrl {
                                image_url: ImageUrl {
                                    url,
                                    detail: detail.or_else(|| Some("auto".to_string())),
                                },
                            }
                        }
                    })
                    .collect();

                Message {
                    role: vm.role,
                    content: Some(MessageContent::Parts(content_parts)),
                    ..Default::default()
                }
            })
            .collect();

        // Utworz ChatCompletionRequest
        let request = ChatCompletionRequest {
            model: model.clone(),
            messages: chat_messages,
            max_tokens,
            temperature: Some(0.0), // Deterministyczna odpowiedz dla OCR
            top_p: None,
            n: None,
            stream: false,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            user: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            rag_options: None,
            memory_options: None,
            audio_input: None,
        };

        // Wyslij POST request (dla formatu openai bezposrednia serializacja, inaczej transformacja)
        let response = if self.needs_transform() {
            let request_body = self.transform_request(&request)?;
            self.client
                .post(url)
                .header("Authorization", self.auth_header_value.as_str())
                .header("Content-Type", "application/json")
                .json(&request_body)
                .send()
                .await
        } else {
            self.client
                .post(url)
                .header("Authorization", self.auth_header_value.as_str())
                .header("Content-Type", "application/json")
                .json(&request)
                .send()
                .await
        }
        .map_err(|e| {
            let error = self.map_reqwest_error(e);
            self.circuit_breaker.record_failure();
            error
        })?;

        let status = response.status();
        debug!("Vision response status: {}", status);

        // Sprawdz status code
        if !status.is_success() {
            if status.is_server_error() {
                self.circuit_breaker.record_failure();
            }
            let error_body = response.text().await.unwrap_or_else(|_| String::new());
            return Err(CoreError::BackendError {
                backend_url: self.url.clone(),
                message: format!("Vision API error ({}): {}", status, error_body),
                source: None,
            }
            .into());
        }

        // Parsuj JSON response
        let completion = if self.request_format.as_deref() == Some("paddleocr") {
            let response_value = response.json::<serde_json::Value>().await.map_err(|e| {
                CoreError::BackendError {
                    backend_url: self.url.clone(),
                    message: format!("Nie mozna sparsowac vision response: {}", e),
                    source: Some(e.into()),
                }
            })?;

            self.transform_response(&response_value)?
        } else {
            response
                .json::<ChatCompletionResponse>()
                .await
                .map_err(|e| CoreError::BackendError {
                    backend_url: self.url.clone(),
                    message: format!("Nie mozna sparsowac vision response: {}", e),
                    source: Some(e.into()),
                })?
        };

        // Wyciagnij tekst z odpowiedzi
        let text = completion
            .choices
            .first()
            .and_then(|choice| {
                choice
                    .message
                    .content
                    .as_ref()
                    .and_then(|content| match content {
                        MessageContent::Text(t) => Some(t.clone()),
                        MessageContent::Parts(parts) => {
                            let mut result = String::new();
                            for part in parts {
                                if let ContentPart::Text { text } = part {
                                    if !result.is_empty() {
                                        result.push('\n');
                                    }
                                    result.push_str(text);
                                }
                            }
                            Some(result)
                        }
                    })
            })
            .unwrap_or_default();

        debug!("Vision zakonczone: {} znakow", text.len());

        self.circuit_breaker.record_success();

        Ok(text)
    }

    /// Zwraca URL backendu (dla logowania i debugowania)
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Zwraca true jesli request wymaga transformacji (np. PaddleOCR)
    fn needs_transform(&self) -> bool {
        self.request_format.as_deref() != Some("openai") && self.request_format.is_some()
    }

    /// Transformuje request do formatu oczekiwanego przez backend.
    ///
    /// Dla roznych backendow (np. PaddleOCR) moze byc potrzebna transformacja
    /// formatu requestu z OpenAI API na niestandardowy format.
    fn transform_request(&self, request: &ChatCompletionRequest) -> Result<serde_json::Value> {
        let format = self.request_format.as_deref().unwrap_or("openai");

        match format {
            "paddleocr" => {
                // Transformacja OpenAI -> PaddleOCR
                // PaddleOCR oczekuje: {"input": [...]}
                // gdzie input to lista elementow z messages[0].content

                debug!("Transformacja requestu: OpenAI -> PaddleOCR");

                // Wyciagnij content z pierwszej wiadomosci
                let first_message =
                    request
                        .messages
                        .first()
                        .ok_or_else(|| CoreError::InternalError {
                            message: "Brak wiadomosci w request".to_string(),
                            source: None,
                        })?;

                // Przeksztalc content do JSON Value
                let mut input_content =
                    serde_json::to_value(&first_message.content).map_err(|e| {
                        CoreError::InternalError {
                            message: format!("Nie mozna serializowac content: {}", e),
                            source: Some(e.into()),
                        }
                    })?;

                // Jesli content jest arrayem, przefiltruj i splaszcz strukture image_url
                if let Some(arr) = input_content.as_array_mut() {
                    let mut filtered_items = Vec::new();

                    for item in arr.iter_mut() {
                        if let Some(obj) = item.as_object_mut() {
                            // Sprawdz typ elementu
                            let item_type = obj.get("type").and_then(|v| v.as_str());

                            // PaddleOCR akceptuje tylko image_url, pomijamy text
                            if item_type == Some("image_url") {
                                // Jesli element ma pole "image_url" z zagniezdzonym "url"
                                if let Some(image_url) =
                                    obj.get("image_url").and_then(|v| v.as_object())
                                {
                                    if let Some(url) = image_url.get("url") {
                                        // Przenieaz url na poziom wyzej
                                        obj.insert("url".to_string(), url.clone());
                                        // Usun zagniezadzone image_url
                                        obj.remove("image_url");
                                    }
                                }
                                filtered_items.push(std::mem::take(item));
                            } else {
                                debug!("Pomijanie elementu typu {:?} dla PaddleOCR", item_type);
                            }
                        }
                    }

                    // Zastap oryginalny array przefiltrowanymi elementami
                    input_content = serde_json::Value::Array(filtered_items);
                }

                // Utworz request PaddleOCR
                let paddleocr_request = serde_json::json!({
                    "input": input_content
                });

                debug!("PaddleOCR request: {}", paddleocr_request);
                Ok(paddleocr_request)
            }
            _ => {
                // Domyslnie: zwroc request bez zmian (OpenAI format)
                serde_json::to_value(request).map_err(|e| {
                    CoreError::InternalError {
                        message: format!("Nie mozna serializowac requestu: {}", e),
                        source: Some(e.into()),
                    }
                    .into()
                })
            }
        }
    }

    /// Transformuje odpowiedz z formatu backendu do formatu OpenAI
    ///
    /// Obsluguje:
    /// - "paddleocr": PaddleOCR -> OpenAI format
    fn transform_response(
        &self,
        response_value: &serde_json::Value,
    ) -> Result<ChatCompletionResponse> {
        debug!("Transformacja odpowiedzi: PaddleOCR -> OpenAI");

        // Wyciagnij dane z PaddleOCR response
        let data = response_value
            .get("data")
            .and_then(|v| v.as_array())
            .ok_or_else(|| CoreError::InternalError {
                message: "Brak pola 'data' w odpowiedzi PaddleOCR".to_string(),
                source: None,
            })?;

        // Przeksztalc text_detections do tekstu
        let mut text_parts = Vec::new();
        for item in data {
            if let Some(detections) = item.get("text_detections").and_then(|v| v.as_array()) {
                for detection in detections {
                    if let Some(text) = detection.get("text").and_then(|v| v.as_str()) {
                        text_parts.push(text.to_string());
                    }
                }
            }
        }

        let content = if text_parts.is_empty() {
            "No text detected".to_string()
        } else {
            text_parts.join("\n")
        };

        // Utworz OpenAI response
        use std::time::{SystemTime, UNIX_EPOCH};

        let completion = ChatCompletionResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            model: self
                .config
                .model_name_override
                .clone()
                .unwrap_or_else(|| "paddleocr".to_string()),
            choices: vec![Choice {
                index: 0,
                message: Message {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text(content)),
                    ..Default::default()
                },
                finish_reason: Some("stop".to_string()),
                logprobs: None,
            }],
            usage: Some(Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            }),
            system_fingerprint: None,
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
            speaker_confidence: None,
            detected_intent: None,
            detected_tools: None,
        };

        debug!("OpenAI response utworzony");
        Ok(completion)
    }
}
