// ============================================================================
// KLIENT QUIC - Natywna biblioteka do komunikacji z TentaFlow.Router
// ============================================================================
//
// CEL:
// Klient QUIC do komunikacji z TentaFlow.Router przez protokół QUIC z TLS.
// Obsługuje wszystkie typy requestów (Embeddings, Completion, RAG, TTS, STT)
// oraz automatyczny reconnect po utracie połączenia.
//
// JAK DZIAŁA:
// 1. Nawiązuje połączenie QUIC z mutual TLS do Router
// 2. Dla każdego requestu otwiera nowy strumień bidirektionalny (multiplexing)
// 3. Serializuje request przez rkyv (zero-copy) i wysyła
// 4. Odbiera odpowiedź i deserializuje przez rkyv
// 5. Przy utracie połączenia automatycznie próbuje reconnect (do 5 prób)
//
// PRZYKŁAD UŻYCIA:
// ```rust
// let config = ClientConfigInternal {
//     router_url: "quic://localhost:3000".into(),
//     cert_path: "certs/cert.pem".into(),
//     key_path: "certs/key.pem".into(),
//     ca_path: "certs/ca.pem".into(),
//     timeout_ms: 30000,
// };
// let client = TentaFlowClient::connect(config).await?;
// let embeddings = client.embeddings("embeddings-gemma", vec!["text".into()]).await?;
// ```
//
// KLUCZOWE KONCEPCJE:
// - QUIC multiplexing: Jedno połączenie obsługuje do 1000 równoległych strumieni
// - Auto-reconnect: Automatyczne ponowne połączenie po utracie (5 prób, 2s interwał)
// - rkyv: Zero-copy serialization dla minimalnej latencji
// - Mutual TLS: Certyfikat klienta + CA dla bezpieczeństwa
// - parking_lot::Mutex: Szybszy mutex (2-3x) dla krótkich sekcji krytycznych
//
// WYDAJNOŚĆ:
// - parking_lot::Mutex zamiast tokio::sync::Mutex dla szybszego dostępu do Connection
// - #[inline] na hot paths dla lepszej optymalizacji przez kompilator
//
// ============================================================================

use anyhow::{Context, Result};
use iroh::endpoint::{Connection, ReadExactError};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tentaflow_protocol::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use parking_lot::Mutex;
use tracing::{debug, info, warn, error};

use tentaflow_transport::{build_client_endpoint, ALPN_SERVICE};

/// Metryki z embeddings.
#[derive(Debug, Clone)]
pub struct EmbeddingsMetrics {
    /// Wektory embeddings
    pub embeddings: Vec<Vec<f32>>,
    /// Wymiary wektora
    pub dimensions: usize,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
    /// Liczba przetworzonych tekstów
    pub texts_count: usize,
}

/// Metryki z non-streaming completion.
#[derive(Debug, Clone)]
pub struct CompletionMetrics {
    /// Tekst odpowiedzi (content)
    pub text: String,
    /// Reasoning content (chain-of-thought) - dla modeli reasoning jak DeepSeek R1, OpenAI o1
    pub reasoning_content: Option<String>,
    /// Przyczyna zakończenia
    pub finish_reason: Option<String>,
    /// Liczba tokenów w prompcie
    pub prompt_tokens: u32,
    /// Liczba tokenów wygenerowanych
    pub completion_tokens: u32,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
    /// Time To First Token w ms (zawsze 0 dla non-streaming)
    pub time_to_first_token_ms: u64,
    /// Tokeny na sekundę
    pub tokens_per_sec: f32,
}

/// Wykryty tool call z Intent Analyzer.
#[derive(Debug, Clone)]
pub struct DetectedToolCallData {
    /// Unikalny identyfikator wywołania
    pub call_id: String,
    /// Nazwa narzędzia (np. "calendar", "email", "web_search")
    pub tool_name: String,
    /// Parametry jako JSON string
    pub parameters: String,
    /// Czy wywołanie jest kompletne
    pub is_complete: bool,
    /// Brakujące parametry
    pub missing_params: Option<Vec<String>>,
    /// Wynik wykonania
    pub execution_result: Option<DetectedToolExecutionResultData>,
    /// Pytanie uzupełniające
    pub follow_up_question: Option<String>,
}

/// Wynik wykonania narzędzia.
#[derive(Debug, Clone)]
pub struct DetectedToolExecutionResultData {
    pub success: bool,
    pub message: String,
    pub data: Option<String>,
    pub error: Option<String>,
}

/// Metryki z streaming completion.
#[derive(Debug, Clone)]
pub struct StreamingMetrics {
    /// Pełny tekst odpowiedzi (content)
    pub text: String,
    /// Reasoning content (chain-of-thought) - dla modeli reasoning jak DeepSeek R1, OpenAI o1
    pub reasoning_content: Option<String>,
    /// Nazwa modelu
    pub model: String,
    /// Liczba tokenów wygenerowanych
    pub completion_tokens: u32,
    /// Time To First Token w ms
    pub time_to_first_token_ms: u64,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
    /// Tokeny na sekundę
    pub tokens_per_sec: f32,
    /// Audio chunks (jeśli TTS był włączony w request)
    pub audio_chunks: Vec<Vec<u8>>,
    /// Transkrybowany tekst z audio input
    pub transcribed_text: Option<String>,
    /// ID rozpoznanego mówcy
    pub speaker_id: Option<String>,
    /// Nazwa rozpoznanego mówcy
    pub speaker_name: Option<String>,
    /// Wykryty intent z Intent Analyzer
    pub detected_intent: Option<String>,
    /// Wykryte tool calls z Intent Analyzer
    pub detected_tools: Option<Vec<DetectedToolCallData>>,
}

/// Opcje dla chat completion - wszystkie opcjonalne parametry w jednej strukturze.
#[derive(Debug, Clone, Default)]
pub struct ChatCompletionOptions {
    /// Temperatura (0.0-2.0)
    pub temperature: Option<f32>,
    /// Maksymalna liczba tokenów
    pub max_tokens: Option<u32>,
    /// Chat template (domyślnie Auto - serwer formatuje)
    pub template: Option<crate::chat_template::ChatTemplate>,
    /// Opcje TTS (None = bez audio)
    pub tts: Option<TTSStreamingOptions>,
    /// Opcje Memory (None = bez pamięci)
    pub memory: Option<MemoryOptions>,
    /// ID sesji (dla Memory i tracking)
    pub session_id: Option<String>,
    /// Czy streamować odpowiedź
    pub stream: bool,
    /// Audio input - jeśli podane, Router przetworzy przez STT
    pub audio_input: Option<Vec<u8>>,
}

/// Dokument zawierający dany chunk.
#[derive(Debug, Clone)]
pub struct ChunkDocumentData {
    /// Identyfikator dokumentu
    pub doc_id: String,
    /// Metadane dokumentu (key-value pairs)
    pub metadata: Vec<(String, String)>,
}

/// Pojedynczy chunk z wyników RAG.
#[derive(Debug, Clone)]
pub struct RagChunkData {
    /// Identyfikator chunka
    pub chunk_id: String,
    /// Treść chunka
    pub chunk_text: String,
    /// Plik źródłowy
    pub source_file: String,
    /// Typ źródła (pdf, docx, txt, etc.)
    pub source_type: String,
    /// Score podobieństwa (0.0-1.0)
    pub similarity_score: f32,
    /// Pozycja w rankingu (1 = najlepszy)
    pub rank: u32,
    /// Indeks chunka w dokumencie
    pub chunk_index: u32,
    /// Lista dokumentów zawierających ten chunk
    pub documents: Vec<ChunkDocumentData>,
}

/// Pełny wynik zapytania RAG.
#[derive(Debug, Clone)]
pub struct RagResponseData {
    /// Tekst odpowiedzi lub kontekst
    pub response: String,
    /// Liczba znalezionych chunków
    pub chunks_found: u32,
    /// Czy wymaga dalszego przetwarzania LLM
    pub requires_llm: bool,
    /// Szczegółowe informacje o chunkach
    pub chunks: Vec<RagChunkData>,
}

/// Segment transkrypcji z metrykami jakości (dla verbose_json).
#[derive(Debug, Clone)]
pub struct SttSegmentData {
    /// ID segmentu
    pub id: u32,
    /// Czas rozpoczęcia w sekundach
    pub start: f32,
    /// Czas zakończenia w sekundach
    pub end: f32,
    /// Tekst segmentu
    pub text: String,
    /// Średnia log probability
    pub avg_logprob: f32,
    /// Prawdopodobieństwo ciszy (no_speech)
    pub no_speech_prob: f32,
    /// Współczynnik kompresji
    pub compression_ratio: f32,
    /// Temperatura użyta
    pub temperature: f32,
    /// Etykieta mówcy z diarization (np. "SPEAKER_00", "Jan Kowalski")
    pub speaker_label: Option<String>,
    /// Similarity score z bazy mówców (0.0-1.0, cosine similarity)
    pub speaker_similarity: Option<f32>,
    /// Czy mówca został rozpoznany z bazy (true) czy to anonimowy speaker (false)
    pub is_known_speaker: Option<bool>,
}

/// Szczegółowy wynik STT z segmentami i metrykami filtrowania.
#[derive(Debug, Clone)]
pub struct SttDetailedData {
    /// Pełny tekst transkrypcji (po filtrowaniu jeśli włączone)
    pub text: String,
    /// Wykryty język (ISO-639-1)
    pub language: Option<String>,
    /// Czas trwania audio w sekundach
    pub duration_seconds: f32,
    /// Segmenty transkrypcji (tylko dla verbose_json)
    pub segments: Vec<SttSegmentData>,
    /// Liczba segmentów odfiltrowanych
    pub filtered_segments_count: u32,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
}

/// Opcje STT dla metody stt_with_options.
#[derive(Debug, Clone, Default)]
pub struct SttOptionsInternal {
    /// Język (ISO-639-1)
    pub language: Option<String>,
    /// Prompt kontekstowy
    pub prompt: Option<String>,
    /// Format odpowiedzi: "json", "text", "verbose_json", "srt", "vtt"
    pub response_format: Option<String>,
    /// Temperatura (0.0-1.0)
    pub temperature: Option<f32>,
    /// Granularność timestampów: "segment" lub "word"
    pub timestamp_granularities: Option<Vec<String>>,
    /// Próg no_speech_prob do filtrowania halucynacji
    pub no_speech_threshold: Option<f32>,
    /// Minimalny avg_logprob dla segmentu
    pub avg_logprob_threshold: Option<f32>,
    /// Maksymalny compression_ratio dla segmentu
    pub compression_ratio_threshold: Option<f32>,
}

/// Klient QUIC do komunikacji z Router.
///
/// Obsługuje automatyczny reconnect po utracie połączenia.
/// Wszystkie metody API automatycznie próbują reconnect jeśli połączenie zostało utracone.
pub struct TentaFlowClient {
    /// Aktywne połączenie QUIC (None jeśli rozłączony)
    connection: Arc<Mutex<Option<Connection>>>,

    /// Endpoint QUIC (do nawiązywania nowych połączeń)
    endpoint: Endpoint,

    /// Konfiguracja klienta (URL, certyfikaty, timeouty)
    config: ClientConfigInternal,

    /// Czy auto-reconnect jest włączony
    auto_reconnect: AtomicBool,

    /// Maksymalna liczba prób reconnect
    max_reconnect_attempts: AtomicU32,

    /// Interwał między próbami reconnect (ms)
    reconnect_interval_ms: AtomicU32,
}

/// Konfiguracja klienta iroh.
#[derive(Clone)]
pub struct ClientConfigInternal {
    /// URL w formacie `iroh://<hex-endpoint-id>` albo czysty hex (32 bajty Ed25519).
    pub router_url: String,

    /// Timeout requestu (ms)
    pub timeout_ms: u64,
}

impl TentaFlowClient {
    /// Tworzy nowego klienta i łączy się z Router.
    ///
    /// Algorytm:
    /// 1. Parsuj URL Router (quic://host:port)
    /// 2. Wczytaj certyfikaty CA (opcjonalne)
    /// 3. Utwórz endpoint QUIC z konfiguracją TLS (one-way)
    /// 4. Nawiąż połączenie z Router
    ///
    /// Parametry:
    /// - `config`: Konfiguracja klienta (URL, opcjonalny CA, timeout)
    ///
    /// Zwraca: Połączony klient lub błąd
    pub async fn connect(config: ClientConfigInternal) -> Result<Self> {
        info!("Łączenie z Router (iroh): {}", config.router_url);

        let endpoint_id = parse_endpoint_id(&config.router_url)
            .context("Niepoprawny URL routera (wymagany `iroh://<hex>` albo 64-znakowy hex)")?;

        let endpoint = build_client_endpoint(vec![ALPN_SERVICE.to_vec()])
            .await
            .context("iroh endpoint bind")?;

        let connection = endpoint
            .connect(EndpointAddr::new(endpoint_id), ALPN_SERVICE)
            .await
            .context("iroh handshake nieudany")?;

        info!("Połączono z Router: endpoint_id={}", endpoint_id.fmt_short());

        Ok(Self {
            connection: Arc::new(Mutex::new(Some(connection))),
            endpoint,
            config,
            auto_reconnect: AtomicBool::new(true),
            max_reconnect_attempts: AtomicU32::new(5),
            reconnect_interval_ms: AtomicU32::new(2000),
        })
    }

    /// Włącza lub wyłącza auto-reconnect.
    pub fn set_auto_reconnect(&self, enabled: bool) {
        self.auto_reconnect.store(enabled, Ordering::Relaxed);
    }

    /// Ustawia maksymalną liczbę prób reconnect.
    pub fn set_max_reconnect_attempts(&self, attempts: u32) {
        self.max_reconnect_attempts.store(attempts, Ordering::Relaxed);
    }

    /// Ustawia interwał między próbami reconnect (ms).
    pub fn set_reconnect_interval(&self, interval_ms: u32) {
        self.reconnect_interval_ms.store(interval_ms, Ordering::Relaxed);
    }

    /// Próbuje ponownie połączyć się z Router.
    ///
    /// Algorytm:
    /// 1. Sprawdź czy auto-reconnect jest włączony
    /// 2. Parsuj URL i rozwiąż adres
    /// 3. Próbuj połączyć się max_reconnect_attempts razy
    /// 4. Między próbami czekaj reconnect_interval_ms
    ///
    /// Zwraca: Ok(()) jeśli połączenie nawiązane, Err jeśli wszystkie próby nieudane
    pub async fn reconnect(&self) -> Result<()> {
        if !self.auto_reconnect.load(Ordering::Relaxed) {
            anyhow::bail!("Auto-reconnect jest wyłączony");
        }

        let max_attempts = self.max_reconnect_attempts.load(Ordering::Relaxed);
        let interval_ms = self.reconnect_interval_ms.load(Ordering::Relaxed);

        let endpoint_id = parse_endpoint_id(&self.config.router_url)
            .context("Niepoprawny URL routera")?;

        for attempt in 1..=max_attempts {
            info!("Próba reconnect {}/{} do {}", attempt, max_attempts, endpoint_id.fmt_short());

            match self
                .endpoint
                .connect(EndpointAddr::new(endpoint_id), ALPN_SERVICE)
                .await
            {
                Ok(connection) => {
                    info!("Reconnect udany po {} próbach", attempt);
                    *self.connection.lock() = Some(connection);
                    return Ok(());
                }
                Err(e) => {
                    warn!("Próba {} nieudana: {}", attempt, e);
                }
            }

            if attempt < max_attempts {
                tokio::time::sleep(std::time::Duration::from_millis(interval_ms as u64)).await;
            }
        }

        error!("Wszystkie {} prób reconnect nieudane", max_attempts);
        anyhow::bail!("Reconnect nieudany po {} próbach", max_attempts)
    }

    /// Pobiera połączenie, próbując reconnect jeśli potrzebne.
    ///
    /// Algorytm:
    /// 1. Sprawdź czy mamy aktywne połączenie
    /// 2. Jeśli nie lub połączenie zamknięte, próbuj reconnect
    /// 3. Zwróć aktywne połączenie
    ///
    /// Performance: parking_lot::Mutex jest ~2-3x szybszy od std::sync::Mutex
    /// dla bardzo krótkich sekcji krytycznych jak klonowanie Connection.
    #[inline]
    async fn get_connection(&self) -> Result<Connection> {
        // Fast path: sprawdź połączenie bez async overhead
        {
            let guard = self.connection.lock();
            if let Some(conn) = guard.as_ref() {
                if conn.close_reason().is_none() {
                    return Ok(conn.clone());
                }
            }
        } // guard dropped here

        // Slow path: reconnect
        if self.auto_reconnect.load(Ordering::Relaxed) {
            warn!("Połączenie utracone, próbuję reconnect...");
            self.reconnect().await?;

            let guard = self.connection.lock();
            guard.clone().ok_or_else(|| anyhow::anyhow!("Brak połączenia po reconnect"))
        } else {
            anyhow::bail!("Brak połączenia i auto-reconnect wyłączony")
        }
    }

    /// Wysyła request embeddings i zwraca wynik z metrykami.
    #[inline]
    pub async fn embeddings(&self, model: &str, texts: Vec<String>) -> Result<EmbeddingsMetrics> {
        let texts_count = texts.len();

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Embeddings(EmbeddingsPayload {
                model: model.to_string(),
                input: texts,
                normalize: true,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(request).await?;

        match response.result {
            ModelResult::Embeddings(result) => {
                let dimensions = result.embeddings.first().map(|v| v.len()).unwrap_or(0);
                let latency_ms = response.metrics
                    .map(|m| m.latency_ms)
                    .unwrap_or(0);

                Ok(EmbeddingsMetrics {
                    embeddings: result.embeddings,
                    dimensions,
                    latency_ms,
                    texts_count,
                })
            }
            ModelResult::Error(e) => anyhow::bail!("Embeddings error: {}", e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Wysyła chat completion request (streaming lub non-streaming).
    ///
    /// # Parametry
    /// - `model`: Nazwa modelu LLM
    /// - `messages`: Lista wiadomości (role, content)
    /// - `options`: Wszystkie opcje (temperature, max_tokens, tts, memory, stream, template, session_id)
    ///
    /// # Callbacki (używane tylko gdy options.stream = true)
    /// - `on_reasoning_start`: Wywoływany gdy zaczyna się reasoning
    /// - `on_reasoning`: Wywoływany dla każdego tokena reasoning
    /// - `on_reasoning_end`: Wywoływany gdy kończy się reasoning
    /// - `on_content_start`: Wywoływany gdy zaczyna się content
    /// - `on_content`: Wywoływany dla każdego tokena content
    /// - `on_content_end`: Wywoływany gdy kończy się content
    /// - `on_audio`: Wywoływany dla każdego audio chunk (jeśli TTS włączony)
    ///
    /// # Zwraca
    /// - (StreamingMetrics, request_id) - metryki i ID requestu dla cancellation
    pub async fn chat_completion<F1, F2, F3, F4, F5, F6, F7>(
        &self,
        model: &str,
        messages: Vec<(String, String)>,
        options: ChatCompletionOptions,
        mut on_reasoning_start: F1,
        mut on_reasoning: F2,
        mut on_reasoning_end: F3,
        mut on_content_start: F4,
        mut on_content: F5,
        mut on_content_end: F6,
        mut on_audio: F7,
    ) -> Result<(StreamingMetrics, String)>
    where
        F1: FnMut(),
        F2: FnMut(&str),
        F3: FnMut(),
        F4: FnMut(),
        F5: FnMut(&str),
        F6: FnMut(),
        F7: FnMut(&[u8]),
    {
        let template = options.template.unwrap_or(crate::chat_template::ChatTemplate::Auto);

        // Formatuj prompt jeśli template nie jest Auto
        let (prompt, msgs, stop) = match template.format(&messages) {
            Some(formatted_prompt) => {
                let stop_tokens = template.stop_tokens();
                (Some(formatted_prompt), Vec::new(), if stop_tokens.is_empty() { None } else { Some(stop_tokens) })
            }
            None => {
                let msgs: Vec<Message> = messages
                    .into_iter()
                    .map(|(role, content)| Message { role, content })
                    .collect();
                (None, msgs, None)
            }
        };

        let request_id = uuid::Uuid::new_v4().to_string();

        // Log TTS options
        let tts_enabled = options.tts.is_some();
        if let Some(ref opts) = options.tts {
            info!("chat_completion: tts_options present - model={}, voice={}, format={:?}",
                opts.model, opts.voice, opts.format);
        }

        let request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Completion(CompletionPayload {
                model: model.to_string(),
                prompt,
                messages: msgs,
                temperature: options.temperature,
                max_tokens: options.max_tokens,
                top_p: None,
                stop,
                presence_penalty: None,
                frequency_penalty: None,
                tts_options: options.tts,
                memory_options: options.memory,
                audio_input: options.audio_input,
                prefix_cache_id: None,
                prefix_text: None,
            }),
            stream: options.stream,
            metadata: None,
            session_id: options.session_id,
        };

        // Pobierz połączenie (z auto-reconnect)
        let conn = self.get_connection().await?;

        // Otwórz strumień bidirektionalny
        let (mut send, mut recv) = conn.open_bi().await?;

        // Serialize request with rkyv
        let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request)
            .map_err(|e| anyhow::anyhow!("Serialization error: {}", e))?;

        info!("Sending request: {} bytes, stream={}, tts={}", request_bytes.len(), options.stream, tts_enabled);

        // Send request
        send.write_all(&request_bytes).await?;
        send.finish()?;

        // === NON-STREAMING MODE ===
        // Serwer zwraca pojedynczą ModelResponse bez length-prefix
        if !options.stream {
            let response_bytes = recv.read_to_end(10_000_000).await?;

            if response_bytes.is_empty() {
                anyhow::bail!("Empty response from server");
            }

            // Deserialize ModelResponse
            let archived = rkyv::access::<ArchivedModelResponse, rkyv::rancor::Error>(&response_bytes)
                .map_err(|e| anyhow::anyhow!("Deserialization error: {}", e))?;

            let response: ModelResponse = rkyv::deserialize::<ModelResponse, rkyv::rancor::Error>(archived)
                .map_err(|e| anyhow::anyhow!("Deserialization error: {}", e))?;

            // Wyciągnij CompletionResult z ModelResponse
            match response.result {
                ModelResult::Completion(result) => {
                    let (latency_ms, completion_tokens, tokens_per_sec) = if let Some(metrics) = response.metrics {
                        (
                            metrics.latency_ms,
                            metrics.tokens_processed.unwrap_or(0) as u32,
                            metrics.throughput_tokens_per_sec.unwrap_or(0.0),
                        )
                    } else {
                        (0, 0, 0.0)
                    };

                    // Wywołaj callbacki dla całego contentu
                    if let Some(ref reasoning) = result.reasoning_content {
                        if !reasoning.is_empty() {
                            on_reasoning_start();
                            on_reasoning(reasoning);
                            on_reasoning_end();
                        }
                    }
                    if !result.text.is_empty() {
                        on_content_start();
                        on_content(&result.text);
                        on_content_end();
                    }

                    // Konwertuj detected_tools z Protocol do lokalnych typów
                    let detected_tools = result.detected_tools.map(|tools| {
                        tools.into_iter().map(|t| DetectedToolCallData {
                            call_id: t.call_id,
                            tool_name: t.tool_name,
                            parameters: t.parameters,
                            is_complete: t.is_complete,
                            missing_params: t.missing_params,
                            execution_result: t.execution_result.map(|er| DetectedToolExecutionResultData {
                                success: er.success,
                                message: er.message,
                                data: er.data,
                                error: er.error,
                            }),
                            follow_up_question: t.follow_up_question,
                        }).collect()
                    });

                    return Ok((StreamingMetrics {
                        text: result.text,
                        reasoning_content: result.reasoning_content,
                        model: result.model,
                        completion_tokens,
                        time_to_first_token_ms: 0, // N/A for non-streaming
                        latency_ms,
                        tokens_per_sec,
                        audio_chunks: vec![],
                        transcribed_text: result.transcribed_text,
                        speaker_id: result.speaker_id,
                        speaker_name: result.speaker_name,
                        detected_intent: result.detected_intent,
                        detected_tools,
                    }, request_id));
                }
                ModelResult::Error(e) => anyhow::bail!("Chat completion error: {}", e.message),
                _ => anyhow::bail!("Unexpected response type for completion"),
            }
        }

        // === STREAMING MODE ===
        // Odbieraj chunki (length-prefixed: 4 bajty BE + dane)
        let mut full_text = String::new();
        let mut full_reasoning = String::new();
        let mut audio_chunks: Vec<Vec<u8>> = Vec::new();
        let mut reasoning_started = false;
        let mut reasoning_ended = false;
        let mut content_started = false;
        // Intent Analyzer info from streaming
        let mut stream_detected_intent: Option<String> = None;
        let mut stream_detected_tools: Option<Vec<DetectedToolCallData>> = None;
        let mut stream_transcribed_text: Option<String> = None;
        let mut stream_speaker_id: Option<String> = None;
        let mut stream_speaker_name: Option<String> = None;

        loop {
            // Odczytaj length prefix (4 bajty)
            let mut len_buf = [0u8; 4];
            match recv.read_exact(&mut len_buf).await {
                Ok(()) => {}
                Err(ReadExactError::FinishedEarly(_)) => {
                    break;
                }
                Err(e) => {
                    anyhow::bail!("Error reading chunk length: {}", e);
                }
            }

            let chunk_len = u32::from_be_bytes(len_buf) as usize;

            if chunk_len == 0 || chunk_len > 10_000_000 {
                anyhow::bail!("Invalid chunk length: {}", chunk_len);
            }

            // Odczytaj chunk data
            let mut chunk_buf = vec![0u8; chunk_len];
            recv.read_exact(&mut chunk_buf).await
                .map_err(|e| anyhow::anyhow!("Error reading chunk data: {}", e))?;

            // Deserializuj ModelStreamChunk
            let archived = rkyv::access::<ArchivedModelStreamChunk, rkyv::rancor::Error>(&chunk_buf)
                .map_err(|e| anyhow::anyhow!("Deserialization error: {}", e))?;

            // Przetwórz chunk type
            match &archived.chunk {
                ArchivedStreamChunkType::TextDelta(text) => {
                    let text_str = text.as_str();

                    // Zakończ reasoning jeśli był i zaczyna się content
                    if reasoning_started && !reasoning_ended {
                        on_reasoning_end();
                        reasoning_ended = true;
                    }

                    // Rozpocznij content jeśli jeszcze nie
                    if !content_started {
                        on_content_start();
                        content_started = true;
                    }

                    full_text.push_str(text_str);
                    on_content(text_str);
                }
                ArchivedStreamChunkType::ReasoningDelta(reasoning) => {
                    let reasoning_str = reasoning.as_str();

                    // Rozpocznij reasoning jeśli jeszcze nie
                    if !reasoning_started {
                        on_reasoning_start();
                        reasoning_started = true;
                    }

                    full_reasoning.push_str(reasoning_str);
                    on_reasoning(reasoning_str);
                }
                ArchivedStreamChunkType::AudioChunk(audio_data) => {
                    // Audio chunk z TTS - wywołaj callback i zapisz
                    let audio_bytes: Vec<u8> = audio_data.iter().copied().collect();
                    on_audio(&audio_bytes);
                    audio_chunks.push(audio_bytes);
                }
                ArchivedStreamChunkType::Done { final_metrics } => {
                    // Zakończ reasoning jeśli nie było content
                    if reasoning_started && !reasoning_ended {
                        on_reasoning_end();
                    }
                    // Zakończ content jeśli był
                    if content_started {
                        on_content_end();
                    }

                    let reasoning_opt = if full_reasoning.is_empty() { None } else { Some(full_reasoning) };
                    let metrics = if let Some(m) = final_metrics.as_ref() {
                        StreamingMetrics {
                            text: full_text,
                            reasoning_content: reasoning_opt,
                            model: m.model_name.to_string(),
                            completion_tokens: m.tokens_processed.as_ref().map(|t| t.to_native() as u32).unwrap_or(0),
                            time_to_first_token_ms: m.time_to_first_token_ms.as_ref().map(|t| t.to_native()).unwrap_or(0),
                            latency_ms: m.latency_ms.to_native(),
                            tokens_per_sec: m.throughput_tokens_per_sec.as_ref().map(|t| t.to_native()).unwrap_or(0.0),
                            audio_chunks,
                            transcribed_text: stream_transcribed_text,
                            speaker_id: stream_speaker_id,
                            speaker_name: stream_speaker_name,
                            detected_intent: stream_detected_intent,
                            detected_tools: stream_detected_tools,
                        }
                    } else {
                        StreamingMetrics {
                            text: full_text,
                            reasoning_content: reasoning_opt,
                            model: model.to_string(),
                            completion_tokens: 0,
                            time_to_first_token_ms: 0,
                            latency_ms: 0,
                            tokens_per_sec: 0.0,
                            audio_chunks,
                            transcribed_text: stream_transcribed_text,
                            speaker_id: stream_speaker_id,
                            speaker_name: stream_speaker_name,
                            detected_intent: stream_detected_intent,
                            detected_tools: stream_detected_tools,
                        }
                    };
                    return Ok((metrics, request_id));
                }
                ArchivedStreamChunkType::Error(err) => {
                    anyhow::bail!("Stream error: {}", err.message.as_str());
                }
                ArchivedStreamChunkType::IntentInfo(info) => {
                    // Zapisz intent info ze streama (używając rkyv ArchivedOption)
                    if let rkyv::option::ArchivedOption::Some(intent) = &info.detected_intent {
                        stream_detected_intent = Some(intent.as_str().to_string());
                    }
                    if let rkyv::option::ArchivedOption::Some(tools) = &info.detected_tools {
                        let converted: Vec<DetectedToolCallData> = tools.iter().map(|t| {
                            DetectedToolCallData {
                                call_id: t.call_id.as_str().to_string(),
                                tool_name: t.tool_name.as_str().to_string(),
                                parameters: t.parameters.as_str().to_string(),
                                is_complete: t.is_complete,
                                missing_params: match &t.missing_params {
                                    rkyv::option::ArchivedOption::Some(mp) => Some(mp.iter().map(|s| s.as_str().to_string()).collect()),
                                    rkyv::option::ArchivedOption::None => None,
                                },
                                execution_result: match &t.execution_result {
                                    rkyv::option::ArchivedOption::Some(er) => Some(DetectedToolExecutionResultData {
                                        success: er.success,
                                        message: er.message.as_str().to_string(),
                                        data: match &er.data {
                                            rkyv::option::ArchivedOption::Some(d) => Some(d.as_str().to_string()),
                                            rkyv::option::ArchivedOption::None => None,
                                        },
                                        error: match &er.error {
                                            rkyv::option::ArchivedOption::Some(e) => Some(e.as_str().to_string()),
                                            rkyv::option::ArchivedOption::None => None,
                                        },
                                    }),
                                    rkyv::option::ArchivedOption::None => None,
                                },
                                follow_up_question: match &t.follow_up_question {
                                    rkyv::option::ArchivedOption::Some(q) => Some(q.as_str().to_string()),
                                    rkyv::option::ArchivedOption::None => None,
                                },
                            }
                        }).collect();
                        stream_detected_tools = Some(converted);
                    }
                    if let rkyv::option::ArchivedOption::Some(text) = &info.transcribed_text {
                        stream_transcribed_text = Some(text.as_str().to_string());
                    }
                    if let rkyv::option::ArchivedOption::Some(id) = &info.speaker_id {
                        stream_speaker_id = Some(id.as_str().to_string());
                    }
                    if let rkyv::option::ArchivedOption::Some(name) = &info.speaker_name {
                        stream_speaker_name = Some(name.as_str().to_string());
                    }
                }
                ArchivedStreamChunkType::Metadata(_) => {}
                _ => {}
            }
        }

        // Zakończ jeśli stream zakończył się bez Done
        if reasoning_started && !reasoning_ended {
            on_reasoning_end();
        }
        if content_started {
            on_content_end();
        }

        let reasoning_opt = if full_reasoning.is_empty() { None } else { Some(full_reasoning) };
        Ok((StreamingMetrics {
            text: full_text,
            reasoning_content: reasoning_opt,
            model: model.to_string(),
            completion_tokens: 0,
            time_to_first_token_ms: 0,
            latency_ms: 0,
            tokens_per_sec: 0.0,
            audio_chunks,
            transcribed_text: stream_transcribed_text,
            speaker_id: stream_speaker_id,
            speaker_name: stream_speaker_name,
            detected_intent: stream_detected_intent,
            detected_tools: stream_detected_tools,
        }, request_id))
    }

    /// Wysyła CancelRequest do Router żeby anulować aktywny streaming request.
    ///
    /// Parametry:
    /// - `request_id`: ID requestu do anulowania
    /// - `reason`: Opcjonalny powód anulowania
    ///
    /// Zwraca: Ok(true) jeśli anulowano, Ok(false) jeśli nie znaleziono
    pub async fn cancel_request(&self, request_id: &str, reason: Option<&str>) -> Result<bool> {
        let cancel = CancelRequest {
            request_id: request_id.to_string(),
            reason: reason.map(|s| s.to_string()),
        };

        // Pobierz połączenie (z auto-reconnect)
        let conn = self.get_connection().await?;

        // Otwórz strumień bidirektionalny
        let (mut send, mut recv) = conn.open_bi().await?;

        // Serialize request with rkyv
        let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&cancel)
            .map_err(|e| anyhow::anyhow!("Serialization error: {}", e))?;

        // Prepend message type discriminator
        let mut payload = Vec::with_capacity(1 + request_bytes.len());
        payload.push(MESSAGE_TYPE_CANCEL_REQUEST);
        payload.extend_from_slice(&request_bytes);

        debug!("Sending cancel request for {}: {} bytes", request_id, payload.len());

        // Send request
        send.write_all(&payload).await?;
        send.finish()?;

        // Receive response
        let response_bytes = recv.read_to_end(10_000).await?;

        if response_bytes.is_empty() {
            anyhow::bail!("Empty response from server");
        }

        // Deserialize response
        let archived = rkyv::access::<ArchivedCancelResponse, rkyv::rancor::Error>(&response_bytes)
            .map_err(|e| anyhow::anyhow!("Deserialization error: {}", e))?;

        Ok(archived.success)
    }

    /// Wysyła request TTS i zwraca audio bytes z metrykami.
    /// Zwraca: (audio_bytes, format, latency_ms, audio_duration_sec)
    pub async fn tts(
        &self,
        model: &str,
        text: &str,
        voice: &str,
        format: Option<&str>,
    ) -> Result<(Vec<u8>, String, u64, f32)> {
        let start = std::time::Instant::now();

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::TTS {
                    model: model.to_string(),
                    input: text.to_string(),
                    voice: voice.to_string(),
                    format: format.map(|s| s.to_string()),
                    speed: None,
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(request).await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        match response.result {
            ModelResult::Audio(result) => {
                match result.data {
                    AudioResultData::Audio(bytes) => {
                        // Oblicz czas trwania audio z WAV (22050Hz, 16-bit, mono)
                        let audio_duration_sec = if bytes.len() > 44 {
                            let data_size = bytes.len() - 44;
                            data_size as f32 / (22050.0 * 2.0)
                        } else {
                            0.0
                        };
                        Ok((bytes, format.unwrap_or("wav").to_string(), latency_ms, audio_duration_sec))
                    }
                    _ => anyhow::bail!("Unexpected audio result type"),
                }
            }
            ModelResult::Error(e) => anyhow::bail!("TTS error: {}", e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Wysyła request STT i zwraca transkrypcję.
    pub async fn stt(
        &self,
        model: &str,
        audio_data: Vec<u8>,
        language: Option<&str>,
    ) -> Result<(String, Option<String>, f32)> {
        eprintln!("[DEBUG] STT: model={}, audio_size={}, language={:?}", model, audio_data.len(), language);
        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::STT {
                    model: model.to_string(),
                    audio_data,
                    language: language.map(|s| s.to_string()),
                    response_format: None,
                    prompt: None,
                    temperature: None,
                    timestamp_granularities: None,
                    no_speech_threshold: None,
                    avg_logprob_threshold: None,
                    compression_ratio_threshold: None,
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(request).await?;

        match response.result {
            ModelResult::Audio(result) => {
                match result.data {
                    AudioResultData::Text(text) => Ok((text, None, 0.0)),
                    AudioResultData::Detailed { text, language, duration, .. } => {
                        Ok((text, Some(language), duration))
                    }
                    _ => anyhow::bail!("Unexpected STT result type"),
                }
            }
            ModelResult::Error(e) => anyhow::bail!("STT error: {}", e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Wysyła request STT z pełnymi opcjami (filtrowanie, verbose_json).
    ///
    /// # Parametry
    /// - `model`: Model Whisper (np. "whisper", "whisper-large")
    /// - `audio_data`: Dane audio (MP3, WAV, M4A, etc.)
    /// - `options`: Opcje STT (język, format, filtrowanie halucynacji)
    ///
    /// # Zwraca
    /// - SttDetailedData z tekstem, segmentami i metrykami
    pub async fn stt_with_options(
        &self,
        model: &str,
        audio_data: Vec<u8>,
        options: SttOptionsInternal,
    ) -> Result<SttDetailedData> {
        let start = std::time::Instant::now();

        eprintln!(
            "[DEBUG] STT with options: model={}, audio_size={}, language={:?}, format={:?}, no_speech_threshold={:?}",
            model, audio_data.len(), options.language, options.response_format, options.no_speech_threshold
        );

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::STT {
                    model: model.to_string(),
                    audio_data,
                    language: options.language,
                    response_format: options.response_format.clone(),
                    prompt: options.prompt,
                    temperature: options.temperature,
                    timestamp_granularities: options.timestamp_granularities,
                    no_speech_threshold: options.no_speech_threshold,
                    avg_logprob_threshold: options.avg_logprob_threshold,
                    compression_ratio_threshold: options.compression_ratio_threshold,
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(request).await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        match response.result {
            ModelResult::Audio(result) => {
                match result.data {
                    AudioResultData::Text(text) => {
                        Ok(SttDetailedData {
                            text,
                            language: None,
                            duration_seconds: 0.0,
                            segments: Vec::new(),
                            filtered_segments_count: 0,
                            latency_ms,
                        })
                    }
                    AudioResultData::Detailed {
                        text,
                        language,
                        duration,
                        segments,
                        filtered_segments_count,
                    } => {
                        // Konwertuj segmenty z Protocol na client
                        let client_segments: Vec<SttSegmentData> = segments
                            .into_iter()
                            .map(|s| SttSegmentData {
                                id: s.id,
                                start: s.start,
                                end: s.end,
                                text: s.text,
                                avg_logprob: s.avg_logprob,
                                no_speech_prob: s.no_speech_prob,
                                compression_ratio: s.compression_ratio,
                                temperature: s.temperature,
                                speaker_label: s.speaker_label,
                                speaker_similarity: s.speaker_similarity,
                                is_known_speaker: s.is_known_speaker,
                            })
                            .collect();

                        Ok(SttDetailedData {
                            text,
                            language: Some(language),
                            duration_seconds: duration,
                            segments: client_segments,
                            filtered_segments_count: filtered_segments_count.unwrap_or(0),
                            latency_ms,
                        })
                    }
                    _ => anyhow::bail!("Unexpected STT result type"),
                }
            }
            ModelResult::Error(e) => anyhow::bail!("STT error: {}", e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    // =========================================================================
    // SPEAKER IDENTIFICATION METHODS
    // =========================================================================

    /// Rejestruje nowego mówcę lub dodaje próbki do istniejącego.
    ///
    /// # Parametry
    /// - `speaker_id`: Unikalny ID mówcy (uuid lub inny string)
    /// - `speaker_name`: Nazwa mówcy (do wyświetlania)
    /// - `audio_samples`: Lista próbek audio (każda próbka jako Vec<u8>)
    /// - `metadata`: Opcjonalne metadane (klucz-wartość)
    ///
    /// # Zwraca
    /// - (speaker_id, speaker_name, samples_processed, embeddings_added, is_new, latency_ms)
    pub async fn speaker_enroll(
        &self,
        speaker_id: &str,
        speaker_name: &str,
        audio_samples: Vec<Vec<u8>>,
        metadata: Option<Vec<(String, String)>>,
    ) -> Result<(String, String, u32, u32, bool, u64)> {
        let start = std::time::Instant::now();

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::SpeakerEnroll {
                    speaker_id: speaker_id.to_string(),
                    speaker_name: speaker_name.to_string(),
                    audio_samples,
                    metadata: metadata.unwrap_or_default(),
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(request).await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        match response.result {
            ModelResult::Audio(result) => {
                match result.data {
                    AudioResultData::SpeakerEnrollResult {
                        speaker_id,
                        speaker_name,
                        samples_processed,
                        embeddings_added,
                        is_new,
                    } => Ok((speaker_id, speaker_name, samples_processed, embeddings_added, is_new, latency_ms)),
                    _ => anyhow::bail!("Unexpected speaker enroll result type"),
                }
            }
            ModelResult::Error(e) => anyhow::bail!("Speaker enroll error: {}", e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Dodaje próbki audio do istniejącego mówcy.
    ///
    /// # Parametry
    /// - `speaker_id`: ID istniejącego mówcy
    /// - `audio_samples`: Lista nowych próbek audio
    ///
    /// # Zwraca
    /// - (speaker_id, speaker_name, samples_processed, embeddings_added, latency_ms)
    pub async fn speaker_add_samples(
        &self,
        speaker_id: &str,
        audio_samples: Vec<Vec<u8>>,
    ) -> Result<(String, String, u32, u32, u64)> {
        let start = std::time::Instant::now();

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::SpeakerAddSamples {
                    speaker_id: speaker_id.to_string(),
                    audio_samples,
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(request).await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        match response.result {
            ModelResult::Audio(result) => {
                match result.data {
                    AudioResultData::SpeakerEnrollResult {
                        speaker_id,
                        speaker_name,
                        samples_processed,
                        embeddings_added,
                        ..
                    } => Ok((speaker_id, speaker_name, samples_processed, embeddings_added, latency_ms)),
                    _ => anyhow::bail!("Unexpected speaker add samples result type"),
                }
            }
            ModelResult::Error(e) => anyhow::bail!("Speaker add samples error: {}", e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Usuwa mówcę z bazy głosów.
    ///
    /// # Parametry
    /// - `speaker_id`: ID mówcy do usunięcia
    ///
    /// # Zwraca
    /// - (speaker_id, success, latency_ms)
    pub async fn speaker_remove(
        &self,
        speaker_id: &str,
    ) -> Result<(String, bool, u64)> {
        let start = std::time::Instant::now();

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::SpeakerRemove {
                    speaker_id: speaker_id.to_string(),
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(request).await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        match response.result {
            ModelResult::Audio(result) => {
                match result.data {
                    AudioResultData::SpeakerRemoveResult { speaker_id, success } => {
                        Ok((speaker_id, success, latency_ms))
                    }
                    _ => anyhow::bail!("Unexpected speaker remove result type"),
                }
            }
            ModelResult::Error(e) => anyhow::bail!("Speaker remove error: {}", e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Pobiera listę wszystkich mówców.
    ///
    /// # Zwraca
    /// - (speakers: Vec<(id, name)>, total_count, latency_ms)
    pub async fn speaker_list(&self) -> Result<(Vec<(String, String)>, u32, u64)> {
        let start = std::time::Instant::now();

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::SpeakerList,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(request).await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        match response.result {
            ModelResult::Audio(result) => {
                match result.data {
                    AudioResultData::SpeakerListResult { speakers, total_count } => {
                        Ok((speakers, total_count, latency_ms))
                    }
                    _ => anyhow::bail!("Unexpected speaker list result type"),
                }
            }
            ModelResult::Error(e) => anyhow::bail!("Speaker list error: {}", e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Pobiera informacje o bazie głosów.
    ///
    /// # Zwraca
    /// - (speaker_count, embedding_dim, similarity_threshold, latency_ms)
    pub async fn speaker_info(&self) -> Result<(u32, u32, f32, u64)> {
        let start = std::time::Instant::now();

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::SpeakerInfo,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(request).await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        match response.result {
            ModelResult::Audio(result) => {
                match result.data {
                    AudioResultData::SpeakerInfoResult {
                        speaker_count,
                        embedding_dim,
                        similarity_threshold,
                    } => Ok((speaker_count, embedding_dim, similarity_threshold, latency_ms)),
                    _ => anyhow::bail!("Unexpected speaker info result type"),
                }
            }
            ModelResult::Error(e) => anyhow::bail!("Speaker info error: {}", e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Identyfikuje mówcę na podstawie próbki audio.
    ///
    /// # Parametry
    /// - `audio_data`: Próbka audio do analizy
    /// - `threshold`: Opcjonalny próg similarity (None = domyślny z konfiguracji)
    ///
    /// # Zwraca
    /// - (is_match, speaker_id, speaker_name, similarity, threshold, latency_ms)
    pub async fn speaker_identify(
        &self,
        audio_data: Vec<u8>,
        threshold: Option<f32>,
    ) -> Result<(bool, Option<String>, Option<String>, f32, f32, u64)> {
        let start = std::time::Instant::now();

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::SpeakerIdentify {
                    audio_data,
                    threshold,
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(request).await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        match response.result {
            ModelResult::Audio(result) => {
                match result.data {
                    AudioResultData::SpeakerIdentifyResult {
                        is_match,
                        speaker_id,
                        speaker_name,
                        similarity,
                        threshold,
                    } => Ok((is_match, speaker_id, speaker_name, similarity, threshold, latency_ms)),
                    _ => anyhow::bail!("Unexpected speaker identify result type"),
                }
            }
            ModelResult::Error(e) => anyhow::bail!("Speaker identify error: {}", e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Weryfikuje czy próbka audio należy do konkretnego mówcy.
    ///
    /// # Parametry
    /// - `speaker_id`: ID mówcy do weryfikacji
    /// - `audio_data`: Próbka audio do analizy
    /// - `threshold`: Opcjonalny próg similarity
    ///
    /// # Zwraca
    /// - (speaker_id, is_verified, similarity, threshold, detected_speaker_id, latency_ms)
    pub async fn speaker_verify(
        &self,
        speaker_id: &str,
        audio_data: Vec<u8>,
        threshold: Option<f32>,
    ) -> Result<(String, bool, f32, f32, Option<String>, u64)> {
        let start = std::time::Instant::now();

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::SpeakerVerify {
                    speaker_id: speaker_id.to_string(),
                    audio_data,
                    threshold,
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(request).await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        match response.result {
            ModelResult::Audio(result) => {
                match result.data {
                    AudioResultData::SpeakerVerifyResult {
                        speaker_id,
                        is_verified,
                        similarity,
                        threshold,
                        detected_speaker_id,
                    } => Ok((speaker_id, is_verified, similarity, threshold, detected_speaker_id, latency_ms)),
                    _ => anyhow::bail!("Unexpected speaker verify result type"),
                }
            }
            ModelResult::Error(e) => anyhow::bail!("Speaker verify error: {}", e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Wysyła request RAG z pełną kontrolą wszystkich parametrów.
    ///
    /// # Parametry
    /// - `query`: Zapytanie użytkownika
    /// - `top_k`: Maksymalna liczba wyników
    /// - `min_similarity`: Minimalny próg podobieństwa (0.0-1.0)
    /// - `search_modes`: Tryby wyszukiwania (None = VectorSearch)
    /// - `use_reranking`: Czy użyć rerankingu (None = false)
    /// - `requires_llm`: Czy przetworzyć przez LLM (None = false)
    /// - `requires_audio`: Czy wygenerować audio (None = false)
    ///
    /// # Zwraca
    /// - RagResponseData z pełnymi informacjami o chunkach
    pub async fn rag(
        &self,
        query: &str,
        top_k: u32,
        min_similarity: f32,
        search_modes: Option<Vec<SearchMode>>,
        use_reranking: Option<bool>,
        requires_llm: Option<bool>,
        requires_audio: Option<bool>,
    ) -> Result<RagResponseData> {
        let modes = search_modes.unwrap_or_else(|| vec![SearchMode::VectorSearch]);

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::RAG(RAGPayload {
                query: query.to_string(),
                context: None,
                params: RAGParams {
                    top_k,
                    min_similarity,
                    use_reranking,
                },
                requires_llm_processing: requires_llm.unwrap_or(false),
                requires_audio_output: requires_audio.unwrap_or(false),
                search_modes: modes,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(request).await?;

        match response.result {
            ModelResult::RAG(result) => {
                let chunks: Vec<RagChunkData> = result.metadata.into_iter().map(|m| {
                    // Konwertuj dokumenty z Protocol na client
                    let documents: Vec<ChunkDocumentData> = m.documents.into_iter().map(|d| {
                        ChunkDocumentData {
                            doc_id: d.doc_id,
                            metadata: d.metadata,
                        }
                    }).collect();

                    RagChunkData {
                        chunk_id: m.chunk_id,
                        chunk_text: m.chunk_text,
                        source_file: m.source_file,
                        source_type: m.source_type,
                        similarity_score: m.similarity_score,
                        rank: m.rank,
                        chunk_index: m.chunk_index,
                        documents,
                    }
                }).collect();

                let chunks_count = chunks.len() as u32;

                Ok(RagResponseData {
                    response: result.context_text,
                    chunks_found: chunks_count,
                    requires_llm: result.requires_llm_processing,
                    chunks,
                })
            }
            ModelResult::Error(e) => anyhow::bail!("RAG error: {}", e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Dodaje dokument do RAG (indeksowanie).
    ///
    /// # Parametry
    /// - `document_id`: Unikalny ID dokumentu
    /// - `content`: Treść dokumentu (tekst lub plik binarny)
    /// - `metadata`: Metadata dokumentu (key-value pairs)
    /// - `index_flags`: Lista indeksów do użycia (puste = wszystkie)
    ///
    /// # Zwraca
    /// - (chunk_count, vector_count, indexed_in) - statystyki indeksowania
    pub async fn ingest_document(
        &self,
        document_id: &str,
        content: DocumentContent,
        metadata: Vec<(String, String)>,
        index_flags: Vec<String>,
    ) -> Result<IngestResponse> {
        let request = IngestRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            document_id: document_id.to_string(),
            content,
            metadata,
            index_flags,
        };

        let response = self.send_ingest_request(request).await?;
        Ok(response)
    }

    /// Dodaje dokument tekstowy do RAG.
    pub async fn ingest_text(
        &self,
        document_id: &str,
        text: &str,
        metadata: Vec<(String, String)>,
    ) -> Result<IngestResponse> {
        self.ingest_document(
            document_id,
            DocumentContent::Text(text.to_string()),
            metadata,
            vec![],
        ).await
    }

    /// Dodaje plik binarny do RAG.
    pub async fn ingest_file(
        &self,
        document_id: &str,
        filename: &str,
        data: Vec<u8>,
        metadata: Vec<(String, String)>,
    ) -> Result<IngestResponse> {
        self.ingest_document(
            document_id,
            DocumentContent::FileData(FileDataContent {
                data,
                filename: filename.to_string(),
            }),
            metadata,
            vec![],
        ).await
    }

    /// Wysyła ModelRequest i odbiera ModelResponse.
    ///
    /// Używa get_connection() więc automatycznie próbuje reconnect jeśli potrzebne.
    #[inline]
    async fn send_request(&self, request: ModelRequest) -> Result<ModelResponse> {
        // Pobierz połączenie (z auto-reconnect)
        let conn = self.get_connection().await?;

        // Otwórz strumień bidirektionalny
        let (mut send, mut recv) = conn.open_bi().await?;

        // Serialize request with rkyv
        let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request)
            .map_err(|e| anyhow::anyhow!("Serialization error: {}", e))?;

        debug!("Sending request: {} bytes", request_bytes.len());

        // Send request
        send.write_all(&request_bytes).await?;
        send.finish()?;

        // Receive response
        let response_bytes = recv.read_to_end(10_000_000).await?;

        debug!("Received response: {} bytes", response_bytes.len());

        if response_bytes.is_empty() {
            anyhow::bail!("Empty response from server");
        }

        // Deserialize response
        let archived = rkyv::access::<ArchivedModelResponse, rkyv::rancor::Error>(&response_bytes)
            .map_err(|e| anyhow::anyhow!("Deserialization error: {}", e))?;

        let response: ModelResponse = rkyv::deserialize::<ModelResponse, rkyv::rancor::Error>(archived)
            .map_err(|e| anyhow::anyhow!("Deserialization error: {}", e))?;

        Ok(response)
    }

    /// Wysyła IngestRequest i odbiera IngestResponse.
    ///
    /// Używa MESSAGE_TYPE_INGEST_REQUEST (0x02) jako bajtu dyskryminatora.
    /// Używa get_connection() więc automatycznie próbuje reconnect jeśli potrzebne.
    async fn send_ingest_request(&self, request: IngestRequest) -> Result<IngestResponse> {
        // Pobierz połączenie (z auto-reconnect)
        let conn = self.get_connection().await?;

        // Otwórz strumień bidirektionalny
        let (mut send, mut recv) = conn.open_bi().await?;

        // Serialize request with rkyv
        let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request)
            .map_err(|e| anyhow::anyhow!("Serialization error: {}", e))?;

        // Debug: pokaż pierwsze bajty serializacji
        let preview_len = std::cmp::min(32, request_bytes.len());
        info!(
            "Serialized IngestRequest: {} bytes, first {}: {:02X?}",
            request_bytes.len(),
            preview_len,
            &request_bytes[..preview_len]
        );

        // Prepend message type discriminator
        let mut payload = Vec::with_capacity(1 + request_bytes.len());
        payload.push(MESSAGE_TYPE_INGEST_REQUEST);
        payload.extend_from_slice(&request_bytes);

        debug!("Sending ingest request: {} bytes", payload.len());

        // Send request
        send.write_all(&payload).await?;
        send.finish()?;

        // Receive response (IngestResponse może być do 100MB dla dużych dokumentów)
        let response_bytes = recv.read_to_end(100 * 1024 * 1024).await?;

        debug!("Received ingest response: {} bytes", response_bytes.len());

        if response_bytes.is_empty() {
            anyhow::bail!("Empty response from server");
        }

        // Deserialize response
        let archived = rkyv::access::<ArchivedIngestResponse, rkyv::rancor::Error>(&response_bytes)
            .map_err(|e| anyhow::anyhow!("Deserialization error: {}", e))?;

        let response: IngestResponse = rkyv::deserialize::<IngestResponse, rkyv::rancor::Error>(archived)
            .map_err(|e| anyhow::anyhow!("Deserialization error: {}", e))?;

        // Check for error status
        if response.status == IngestionStatus::Error {
            let error_msg = response.error.clone().unwrap_or_else(|| "Unknown error".to_string());
            anyhow::bail!("Ingest error: {}", error_msg);
        }

        Ok(response)
    }

    /// Zamyka połączenie.
    #[inline]
    pub async fn close(&self) {
        if let Some(conn) = self.connection.lock().take() {
            conn.close(0u32.into(), b"client closing");
        }
    }

    /// Sprawdza czy połączenie jest aktywne.
    ///
    /// Performance: Branchless check with map + unwrap_or pattern.
    #[inline]
    pub async fn is_connected(&self) -> bool {
        self.connection.lock()
            .as_ref()
            .map(|conn| conn.close_reason().is_none())
            .unwrap_or(false)
    }

    // =========================================================================
    // CONVERSATION SESSIONS
    // =========================================================================

    /// Rozpoczyna nową sesję konwersacji głosowej.
    /// Zwraca (session_id, initial_state).
    pub async fn conversation_start(
        &self,
        mode: u8,
        _user_id: Option<String>,
        _language: Option<String>,
        _stt_model: Option<String>,
        wake_words: Vec<String>,
        stop_phrases: Vec<String>,
        silence_timeout_ms: u32,
        _pre_wake_buffer_ms: u32,
    ) -> Result<(String, u8)> {
        use tentaflow_protocol::{
            AudioOperation, AudioPayload, ModelPayload, ModelRequest, ModelResult,
            AudioResultData, SessionMode, ConversationSessionConfig as ProtoConfig,
        };

        let session_mode = match mode {
            0 => SessionMode::AlwaysOn,
            1 => SessionMode::WakeWordTimeout { silence_timeout_ms },
            2 => SessionMode::WakeWordExplicitStop,
            _ => SessionMode::WakeWordTimeout { silence_timeout_ms: 30000 },
        };

        // Build protocol config with default values for fields not exposed in FFI
        let config = ProtoConfig {
            mode: session_mode,
            wake_words,
            stop_phrases,
            wake_word_sensitivity: 0.5,
            vad_enabled: true,
            vad_threshold: 0.5,
            play_activation_sound: false,
            play_deactivation_sound: false,
        };

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::ConversationStart { config },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(request).await?;

        match response.result {
            ModelResult::Audio(audio_result) => {
                if let AudioResultData::ConversationStartResult {
                    session_id,
                    success: _,
                    initial_state,
                    config: _,
                    message: _
                } = audio_result.data {
                    let state_byte = match initial_state {
                        tentaflow_protocol::SessionState::Inactive => 0,
                        tentaflow_protocol::SessionState::Active => 1,
                        tentaflow_protocol::SessionState::Processing => 2,
                        tentaflow_protocol::SessionState::Speaking => 3,
                    };
                    Ok((session_id, state_byte))
                } else {
                    anyhow::bail!("Unexpected response type for ConversationStart")
                }
            }
            ModelResult::Error(e) => anyhow::bail!("{:?}: {}", e.error_type, e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Wysyła audio do aktywnej sesji konwersacji.
    /// Zwraca (state, events, transcription, confidence).
    pub async fn conversation_audio(
        &self,
        session_id: &str,
        audio_data: Vec<u8>,
        timestamp_ms: u64,
    ) -> Result<(u8, Vec<crate::types::ConversationEvent>, Option<String>, f32)> {
        use tentaflow_protocol::{
            AudioOperation, AudioPayload, ModelPayload, ModelRequest, ModelResult,
            AudioResultData, ConversationEvent as ProtoEvent, ActivationReason, DeactivationReason,
        };
        use crate::types::ConversationEvent;
        use std::ffi::CString;

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::ConversationAudio {
                    session_id: session_id.to_string(),
                    audio_data,
                    timestamp_ms,
                },
            }),
            stream: false,
            metadata: None,
            session_id: Some(session_id.to_string()),
        };

        let response = self.send_request(request).await?;

        match response.result {
            ModelResult::Audio(audio_result) => {
                // Server can return either new ConversationAudioResult or old ConversationEventResult
                match audio_result.data {
                    // New format: ConversationAudioResult with all info in one response
                    AudioResultData::ConversationAudioResult {
                        session_id: _,
                        state,
                        transcription,
                        confidence,
                        events,
                    } => {
                        let state_byte = match state {
                            tentaflow_protocol::SessionState::Inactive => 0u8,
                            tentaflow_protocol::SessionState::Active => 1u8,
                            tentaflow_protocol::SessionState::Processing => 2u8,
                            tentaflow_protocol::SessionState::Speaking => 3u8,
                        };

                        // Convert protocol events to FFI events
                        let ffi_events: Vec<ConversationEvent> = events.iter().map(|event_data| {
                            let (event_type, ev_transcription, ev_confidence, wake_word, stop_phrase) = match event_data {
                                ProtoEvent::SessionActivated { activation_reason, .. } => {
                                    let wake = match activation_reason {
                                        ActivationReason::WakeWord { detected_phrase, .. } => Some(detected_phrase.clone()),
                                        _ => None,
                                    };
                                    (0u8, None, 0.0f32, wake, None)
                                }
                                ProtoEvent::SessionDeactivated { deactivation_reason, .. } => {
                                    match deactivation_reason {
                                        DeactivationReason::SilenceTimeout { .. } => (3u8, None, 0.0f32, None, None),
                                        DeactivationReason::StopPhrase { detected_phrase } => (4u8, None, 0.0f32, None, Some(detected_phrase.clone())),
                                        _ => (5u8, None, 0.0f32, None, None),
                                    }
                                }
                                ProtoEvent::SpeechStarted { .. } => (0u8, None, 0.0f32, None, None),
                                ProtoEvent::SpeechEnded { .. } => (5u8, None, 0.0f32, None, None),
                                ProtoEvent::UtteranceTranscribed { text, confidence, .. } => {
                                    (2u8, Some(text.clone()), *confidence, None, None)
                                }
                                ProtoEvent::TimeoutWarning { .. } => (3u8, None, 0.0f32, None, None),
                                ProtoEvent::SessionError { error_message, .. } => {
                                    (5u8, Some(error_message.clone()), 0.0f32, None, None)
                                }
                            };

                            ConversationEvent {
                                event_type,
                                timestamp_ms: 0,
                                transcription: ev_transcription
                                    .map(|s| CString::new(s).unwrap_or_default().into_raw())
                                    .unwrap_or(std::ptr::null_mut()),
                                confidence: ev_confidence,
                                wake_word: wake_word
                                    .map(|s| CString::new(s).unwrap_or_default().into_raw())
                                    .unwrap_or(std::ptr::null_mut()),
                                stop_phrase: stop_phrase
                                    .map(|s| CString::new(s).unwrap_or_default().into_raw())
                                    .unwrap_or(std::ptr::null_mut()),
                                user_id: std::ptr::null_mut(),
                            }
                        }).collect();

                        Ok((state_byte, ffi_events, transcription, confidence.unwrap_or(0.0)))
                    }

                    // Old format: ConversationEventResult for single event
                    AudioResultData::ConversationEventResult {
                        session_id: _,
                        event_data,
                        timestamp_ms: event_ts,
                    } => {
                        // Convert single protocol event to FFI event
                        let (event_type, transcription, confidence, wake_word, stop_phrase) = match &event_data {
                            ProtoEvent::SessionActivated { activation_reason, .. } => {
                                let wake = match activation_reason {
                                    ActivationReason::WakeWord { detected_phrase, .. } => Some(detected_phrase.clone()),
                                    _ => None,
                                };
                                (0u8, None, 0.0f32, wake, None)
                            }
                            ProtoEvent::SessionDeactivated { deactivation_reason, .. } => {
                                match deactivation_reason {
                                    DeactivationReason::SilenceTimeout { .. } => (3u8, None, 0.0f32, None, None),
                                    DeactivationReason::StopPhrase { detected_phrase } => (4u8, None, 0.0f32, None, Some(detected_phrase.clone())),
                                    _ => (5u8, None, 0.0f32, None, None),
                                }
                            }
                            ProtoEvent::SpeechStarted { .. } => (0u8, None, 0.0f32, None, None),
                            ProtoEvent::SpeechEnded { .. } => (5u8, None, 0.0f32, None, None),
                            ProtoEvent::UtteranceTranscribed { text, confidence, .. } => {
                                (2u8, Some(text.clone()), *confidence, None, None)
                            }
                            ProtoEvent::TimeoutWarning { .. } => (3u8, None, 0.0f32, None, None),
                            ProtoEvent::SessionError { error_message, .. } => {
                                (5u8, Some(error_message.clone()), 0.0f32, None, None)
                            }
                        };

                        let state_byte = match &event_data {
                            ProtoEvent::SessionActivated { .. } => 1u8,
                            ProtoEvent::SessionDeactivated { .. } => 0u8,
                            ProtoEvent::SpeechStarted { .. } => 2u8,
                            ProtoEvent::SpeechEnded { .. } => 1u8,
                            ProtoEvent::UtteranceTranscribed { .. } => 1u8,
                            ProtoEvent::TimeoutWarning { .. } => 1u8,
                            ProtoEvent::SessionError { .. } => 0u8,
                        };

                        let ffi_event = ConversationEvent {
                            event_type,
                            timestamp_ms: event_ts,
                            transcription: transcription.clone()
                                .map(|s| CString::new(s).unwrap_or_default().into_raw())
                                .unwrap_or(std::ptr::null_mut()),
                            confidence,
                            wake_word: wake_word
                                .map(|s| CString::new(s).unwrap_or_default().into_raw())
                                .unwrap_or(std::ptr::null_mut()),
                            stop_phrase: stop_phrase
                                .map(|s| CString::new(s).unwrap_or_default().into_raw())
                                .unwrap_or(std::ptr::null_mut()),
                            user_id: std::ptr::null_mut(),
                        };

                        Ok((state_byte, vec![ffi_event], transcription, confidence))
                    }

                    // No event for this audio chunk - return empty
                    _ => Ok((1u8, vec![], None, 0.0))
                }
            }
            ModelResult::Error(e) => anyhow::bail!("{:?}: {}", e.error_type, e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Kończy sesję konwersacji.
    /// Zwraca (final_transcription, stats).
    pub async fn conversation_end(
        &self,
        session_id: &str,
        reason: Option<String>,
    ) -> Result<(Option<String>, crate::types::ConversationSessionStats)> {
        use tentaflow_protocol::{
            AudioOperation, AudioPayload, ModelPayload, ModelRequest, ModelResult,
            AudioResultData,
        };
        use crate::types::ConversationSessionStats;

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::ConversationEnd {
                    session_id: session_id.to_string(),
                    reason,
                },
            }),
            stream: false,
            metadata: None,
            session_id: Some(session_id.to_string()),
        };

        let response = self.send_request(request).await?;

        match response.result {
            ModelResult::Audio(audio_result) => {
                if let AudioResultData::ConversationEndResult {
                    session_id: _,
                    success: _,
                    stats,
                } = audio_result.data {
                    // Map protocol stats to FFI stats
                    let ffi_stats = ConversationSessionStats {
                        total_duration_ms: stats.total_duration_ms,
                        active_speech_ms: stats.active_duration_ms,
                        wake_words_detected: stats.wake_word_detections,
                        transcriptions_count: stats.utterance_count,
                        speakers_detected: 0, // Not available in protocol stats
                    };
                    // Note: final_transcription not available in protocol ConversationEndResult
                    Ok((None, ffi_stats))
                } else {
                    anyhow::bail!("Unexpected response type for ConversationEnd")
                }
            }
            ModelResult::Error(e) => anyhow::bail!("{:?}: {}", e.error_type, e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Pobiera status sesji konwersacji.
    /// Zwraca (exists, state, mode, duration_ms, last_activity_ms).
    pub async fn conversation_status(
        &self,
        session_id: &str,
    ) -> Result<(bool, u8, u8, u64, u64)> {
        use tentaflow_protocol::{
            AudioOperation, AudioPayload, ModelPayload, ModelRequest, ModelResult,
            AudioResultData,
        };

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::ConversationStatus {
                    session_id: session_id.to_string(),
                },
            }),
            stream: false,
            metadata: None,
            session_id: Some(session_id.to_string()),
        };

        let response = self.send_request(request).await?;

        match response.result {
            ModelResult::Audio(audio_result) => {
                if let AudioResultData::ConversationStatusResult {
                    session_id: _,
                    exists,
                    info,
                } = audio_result.data {
                    if let Some(session_info) = info {
                        let state_byte = match session_info.state {
                            tentaflow_protocol::SessionState::Inactive => 0,
                            tentaflow_protocol::SessionState::Active => 1,
                            tentaflow_protocol::SessionState::Processing => 2,
                            tentaflow_protocol::SessionState::Speaking => 3,
                        };
                        let mode_byte = match session_info.mode {
                            tentaflow_protocol::SessionMode::AlwaysOn => 0,
                            tentaflow_protocol::SessionMode::WakeWordTimeout { .. } => 1,
                            tentaflow_protocol::SessionMode::WakeWordExplicitStop => 2,
                        };
                        // started_at_ms is actually elapsed time since session creation (not a timestamp)
                        let duration_ms = session_info.started_at_ms;
                        Ok((exists, state_byte, mode_byte, duration_ms, session_info.last_activity_ms))
                    } else {
                        // Session doesn't exist
                        Ok((false, 0, 0, 0, 0))
                    }
                } else {
                    anyhow::bail!("Unexpected response type for ConversationStatus")
                }
            }
            ModelResult::Error(e) => anyhow::bail!("{:?}: {}", e.error_type, e.message),
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

}

/// Parsuje URL routera w formacie `iroh://<hex>` albo czysty 64-znakowy hex.
fn parse_endpoint_id(url: &str) -> Result<EndpointId> {
    let raw = url.trim();
    let hex_str = raw
        .strip_prefix("iroh://")
        .unwrap_or(raw)
        .trim_end_matches('/')
        .trim_start_matches("0x");

    let bytes = hex::decode(hex_str)
        .map_err(|e| anyhow::anyhow!("hex EndpointId: {e}"))?;
    if bytes.len() != 32 {
        anyhow::bail!("EndpointId musi miec 32 bajty, ma {}", bytes.len());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    EndpointId::from_bytes(&arr).map_err(|e| anyhow::anyhow!("niepoprawny EndpointId: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ingest_request_roundtrip() {
        // Create IngestRequest exactly as the client does
        let request = IngestRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            document_id: "test-doc-123".to_string(),
            content: DocumentContent::Text("Test document content for RAG".to_string()),
            metadata: vec![
                ("source".to_string(), "test".to_string()),
            ],
            index_flags: vec![],
        };

        // Serialize with rkyv (same as send_ingest_request)
        let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request)
            .expect("Failed to serialize IngestRequest");

        println!("Client test: serialized {} bytes", request_bytes.len());
        println!("First 40 bytes: {:02X?}", &request_bytes[..std::cmp::min(40, request_bytes.len())]);

        // Deserialize with access (same as Router does)
        let archived = rkyv::access::<ArchivedIngestRequest, rkyv::rancor::Error>(&request_bytes)
            .expect("Failed to access ArchivedIngestRequest");

        // Verify
        assert_eq!(archived.request_id.as_str(), request.request_id);
        assert_eq!(archived.document_id.as_str(), request.document_id);

        // Full deserialize
        let deserialized: IngestRequest = rkyv::deserialize::<IngestRequest, rkyv::rancor::Error>(archived)
            .expect("Failed to deserialize IngestRequest");

        assert_eq!(deserialized.request_id, request.request_id);
        assert_eq!(deserialized.metadata.len(), 1);

        println!("Client roundtrip test: SUCCESS!");
    }
}
