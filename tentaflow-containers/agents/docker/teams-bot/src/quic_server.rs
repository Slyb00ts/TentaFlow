// =============================================================================
// Plik: quic_server.rs
// Opis: Serwer iroh kontenera meeting bot. Router laczy sie po `EndpointId`,
//       wysyla `ModelRequest` w length-prefixed rkyv; kontener odpowiada
//       `ModelResponse` albo strumieniuje `ModelStreamChunk`. Z tego samego
//       `Connection` kontener moze inicjowac `accept_bi → open_bi` w odwrotna
//       strone — `RouterClient` wysyla STT/TTS requesty do routera.
// =============================================================================

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use iroh::endpoint::Connection;
use iroh::SecretKey;
use tentaflow_protocol::*;
use tentaflow_transport::{
    build_server_endpoint, read_frame, write_frame, ServerEndpointConfig, ALPN_SERVICE,
};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, error, info, warn};

/// Komenda sterujaca spotkaniem — wysylana z iroh do glownej petli w main.rs
pub enum MeetingCommand {
    JoinMeeting {
        meeting_url: String,
        response_tx: oneshot::Sender<String>,
    },
    LeaveMeeting {
        response_tx: oneshot::Sender<String>,
    },
    GetStatus {
        response_tx: oneshot::Sender<String>,
    },
}

/// Klient do wysylania requestow z sidecara do routera na istniejacym `Connection`.
/// Sidecar trzyma `Connection` ktore router nawiazal i `open_bi` w druga strone
/// otwiera nowy bidi stream obslugiwany przez routera (reverse_listener).
///
/// `current_meeting_id` ustawiane przy JoinMeeting, uzywane w ModelRequest.metadata
/// zeby router wiedzial do ktorego meetingu przypisac STT/diarization.
pub struct RouterClient {
    connection: Connection,
    current_meeting_id: Arc<parking_lot::Mutex<Option<String>>>,
}

/// Wynik wywolania `RouterClient::chat_completion` — text + rozwiazana nazwa
/// modelu (po alias resolution po stronie routera).
pub struct ChatCompletionResult {
    pub content: String,
    pub resolved_model: String,
}

impl RouterClient {
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            current_meeting_id: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    pub fn set_meeting_id(&self, meeting_id: String) {
        *self.current_meeting_id.lock() = Some(meeting_id);
    }

    pub fn clear_meeting_id(&self) {
        *self.current_meeting_id.lock() = None;
    }

    pub fn current_meeting_id(&self) -> Option<String> {
        self.current_meeting_id.lock().clone()
    }

    /// Wysyla `ModelRequest` do routera i czeka na `ModelResponse`. Format
    /// ramki w obu kierunkach: `[u32 BE length][rkyv payload]`.
    pub async fn send_request(&self, request: &ModelRequest) -> Result<ModelResponse> {
        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|e| anyhow::anyhow!("open_bi do routera: {e}"))?;

        write_frame(&mut send, request)
            .await
            .map_err(|e| anyhow::anyhow!("write ModelRequest: {e}"))?;
        send.finish().ok();

        read_frame::<ModelResponse>(&mut recv)
            .await
            .map_err(|e| anyhow::anyhow!("read ModelResponse: {e}"))?
            .ok_or_else(|| anyhow::anyhow!("router zamknal stream bez odpowiedzi"))
    }

    /// Wysyla audio do STT przez router (alias podany w `model`).
    /// `extra_metadata` — dodatkowe pary klucz/wartosc (roster, active_speaker,
    /// timestamp_ms) — doklejane obok `meeting_id`.
    pub async fn transcribe(
        &self,
        audio_pcm: &[i16],
        model: &str,
        language: Option<String>,
        extra_metadata: Vec<(String, String)>,
    ) -> Result<String> {
        let audio_bytes: Vec<u8> = audio_pcm.iter().flat_map(|s| s.to_le_bytes()).collect();

        let mut meta: Vec<(String, String)> = Vec::new();
        if let Some(mid) = self.current_meeting_id() {
            meta.push(("meeting_id".to_string(), mid));
        }
        meta.extend(extra_metadata);
        let metadata = if meta.is_empty() { None } else { Some(meta) };

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::STT {
                    model: model.to_string(),
                    audio_data: audio_bytes,
                    language,
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
            metadata,
            session_id: None,
        };

        let response = self.send_request(&request).await?;
        match response.result {
            ModelResult::Audio(audio) => match audio.data {
                AudioResultData::Text(text) => Ok(text),
                AudioResultData::Detailed { text, .. } => Ok(text),
                _ => anyhow::bail!("STT zwrocil nieoczekiwany typ danych"),
            },
            ModelResult::Error(e) => anyhow::bail!("STT blad: {}", e.message),
            _ => anyhow::bail!("Nieoczekiwany typ odpowiedzi STT"),
        }
    }

    /// Wysyla chat.completions request przez router. Router rozwiazuje alias
    /// -> konkretny model i zwraca wygenerowany tekst + rozwiazana nazwa modelu.
    /// Uzywane przez summarizer do generowania podsumowan transkryptu.
    pub async fn chat_completion(
        &self,
        model_alias: &str,
        messages: Vec<(String, String)>,
    ) -> Result<ChatCompletionResult> {
        let msgs: Vec<Message> = messages
            .into_iter()
            .map(|(role, content)| Message { role, content })
            .collect();

        let mut meta: Vec<(String, String)> = Vec::new();
        if let Some(mid) = self.current_meeting_id() {
            meta.push(("meeting_id".to_string(), mid));
        }
        let metadata = if meta.is_empty() { None } else { Some(meta) };

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Completion(CompletionPayload {
                model: model_alias.to_string(),
                prompt: None,
                messages: msgs,
                temperature: Some(0.2),
                max_tokens: Some(1024),
                top_p: None,
                stop: None,
                presence_penalty: None,
                frequency_penalty: None,
                tts_options: None,
                memory_options: None,
                audio_input: None,
                prefix_cache_id: None,
                prefix_text: None,
            }),
            stream: false,
            metadata,
            session_id: None,
        };

        let response = self.send_request(&request).await?;
        match response.result {
            ModelResult::Completion(c) => Ok(ChatCompletionResult {
                content: c.text,
                resolved_model: c.model,
            }),
            ModelResult::Error(e) => anyhow::bail!("chat_completion blad: {}", e.message),
            _ => anyhow::bail!("Nieoczekiwany typ odpowiedzi chat_completion"),
        }
    }

    /// Pobiera treść promptu z DB routera po `prompt_id` + język. Router robi
    /// fallback na `pl` jeśli wariant w żądanym języku nie istnieje.
    /// Zwraca treść promptu (pole `content`) — `name` i `resolved_language`
    /// są ignorowane, bo bot potrzebuje tylko treści do system message.
    pub async fn fetch_prompt(&self, prompt_id: &str, language: &str) -> Result<String> {
        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::PromptFetch(PromptFetchRequest {
                prompt_id: prompt_id.to_string(),
                language: language.to_string(),
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(&request).await?;
        match response.result {
            ModelResult::PromptFetched(p) => Ok(p.content),
            ModelResult::Error(e) => anyhow::bail!("PromptFetch blad: {}", e.message),
            _ => anyhow::bail!("Nieoczekiwany typ odpowiedzi PromptFetch"),
        }
    }

    /// Wysyla MeetingEvent (SummaryUpdate albo ActionItemsUpdate) do routera.
    /// Router persistuje w tabelach `meeting_summaries` / `meeting_action_items`
    /// przez `persist_meeting_event` w reverse_request.rs.
    pub async fn send_meeting_event(
        &self,
        meeting_key: &str,
        timestamp_ms: i64,
        payload: MeetingEventPayload,
    ) -> Result<()> {
        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::MeetingEvent(MeetingEventData {
                meeting_key: meeting_key.to_string(),
                timestamp_ms,
                payload,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(&request).await?;
        match response.result {
            ModelResult::Error(e) => anyhow::bail!("MeetingEvent blad: {}", e.message),
            _ => Ok(()),
        }
    }

    /// Wysyla tekst do TTS przez router (alias podany w `model`).
    pub async fn synthesize(
        &self,
        text: &str,
        voice: &str,
        model: &str,
    ) -> Result<Vec<u8>> {
        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::TTS {
                    model: model.to_string(),
                    input: text.to_string(),
                    voice: voice.to_string(),
                    format: Some("pcm".to_string()),
                    speed: None,
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(&request).await?;
        match response.result {
            ModelResult::Audio(audio) => match audio.data {
                AudioResultData::Audio(bytes) => Ok(bytes),
                _ => anyhow::bail!("TTS zwrocil nieoczekiwany typ danych"),
            },
            ModelResult::Error(e) => anyhow::bail!("TTS blad: {}", e.message),
            _ => anyhow::bail!("Nieoczekiwany typ odpowiedzi TTS"),
        }
    }
}

/// Konfiguracja serwera iroh kontenera
#[derive(Debug, Clone)]
pub struct ContainerTransportConfig {
    pub port: u16,
    pub secret_key_path: Option<String>,
    pub secret_key_hex: Option<String>,
    pub enable_lan_discovery: bool,
    pub enable_dht_discovery: bool,
}

impl Default for ContainerTransportConfig {
    fn default() -> Self {
        Self {
            port: 5000,
            secret_key_path: None,
            secret_key_hex: None,
            enable_lan_discovery: true,
            enable_dht_discovery: true,
        }
    }
}

/// Serwer iroh meeting bota.
pub struct MeetingQuicServer {
    config: ContainerTransportConfig,
    transcript_tx: mpsc::UnboundedSender<(String, String, u64)>,
    transcript_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String, u64)>>>,
    router_client: Arc<tokio::sync::Mutex<Option<Arc<RouterClient>>>>,
    command_tx: mpsc::UnboundedSender<MeetingCommand>,
    command_rx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<MeetingCommand>>>>,
}

impl MeetingQuicServer {
    pub fn new(config: ContainerTransportConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        Self {
            config,
            transcript_tx: tx,
            transcript_rx: Arc::new(tokio::sync::Mutex::new(rx)),
            router_client: Arc::new(tokio::sync::Mutex::new(None)),
            command_tx: cmd_tx,
            command_rx: Arc::new(tokio::sync::Mutex::new(Some(cmd_rx))),
        }
    }

    pub fn transcript_sender(&self) -> mpsc::UnboundedSender<(String, String, u64)> {
        self.transcript_tx.clone()
    }

    pub async fn command_receiver(&self) -> Option<mpsc::UnboundedReceiver<MeetingCommand>> {
        self.command_rx.lock().await.take()
    }

    pub async fn router_client(&self) -> Option<Arc<RouterClient>> {
        self.router_client.lock().await.clone()
    }

    pub fn router_client_handle(&self) -> Arc<tokio::sync::Mutex<Option<Arc<RouterClient>>>> {
        self.router_client.clone()
    }

    /// Uruchamia iroh endpoint i nasluchuje na polaczenia od routera.
    pub async fn run(&self, mut shutdown_rx: watch::Receiver<bool>) -> Result<()> {
        let secret_key = if let Some(hex) = self.config.secret_key_hex.as_deref() {
            load_secret_key_from_hex(hex)?
        } else {
            load_or_generate_secret_key(self.config.secret_key_path.as_deref())?
        };
        let bind_addr: SocketAddr = format!("0.0.0.0:{}", self.config.port)
            .parse()
            .context("Nieprawidlowy bind addr")?;

        let endpoint = build_server_endpoint(ServerEndpointConfig {
            secret_key,
            bind_addr,
            alpns: vec![ALPN_SERVICE.to_vec()],
            relay_url: None,
            enable_lan_discovery: self.config.enable_lan_discovery,
            enable_dht_discovery: self.config.enable_dht_discovery,
        })
        .await
        .context("iroh endpoint bind")?;

        info!(
            endpoint_id = %endpoint.id().fmt_short(),
            port = self.config.port,
            "Meeting bot iroh endpoint nasluchuje"
        );

        loop {
            tokio::select! {
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else {
                        warn!("iroh endpoint zwrocil None");
                        break;
                    };

                    let transcript_rx = self.transcript_rx.clone();
                    let router_client_slot = self.router_client.clone();
                    let command_tx = self.command_tx.clone();

                    tokio::spawn(async move {
                        match incoming.await {
                            Ok(connection) => {
                                let remote = connection.remote_id();
                                debug!(remote = %remote.fmt_short(), "Router polaczony");
                                Self::handle_connection(
                                    connection,
                                    transcript_rx,
                                    router_client_slot,
                                    command_tx,
                                ).await;
                            }
                            Err(e) => {
                                error!("iroh handshake nieudany: {e}");
                            }
                        }
                    });
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("Zamykam iroh endpoint...");
                        endpoint.close().await;
                        info!("iroh endpoint zamkniety");
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Obsluguje pojedyncze polaczenie od routera.
    async fn handle_connection(
        connection: Connection,
        transcript_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String, u64)>>>,
        router_client_slot: Arc<tokio::sync::Mutex<Option<Arc<RouterClient>>>>,
        command_tx: mpsc::UnboundedSender<MeetingCommand>,
    ) {
        let remote = connection.remote_id();

        // Zapisz RouterClient — to samo polaczenie sluzy do przyjmowania
        // requestow (accept_bi) i do wysylania requestow (open_bi).
        let client = Arc::new(RouterClient::new(connection.clone()));
        {
            let mut slot = router_client_slot.lock().await;
            *slot = Some(client);
        }
        info!(remote = %remote.fmt_short(), "RouterClient gotowy");

        loop {
            match connection.accept_bi().await {
                Ok((send, recv)) => {
                    let transcript_rx = transcript_rx.clone();
                    let command_tx = command_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_stream(send, recv, transcript_rx, command_tx).await {
                            debug!("handle_stream: {e}");
                        }
                    });
                }
                Err(e) => {
                    debug!(remote = %remote.fmt_short(), "Polaczenie zamkniete: {e}");
                    break;
                }
            }
        }

        let mut slot = router_client_slot.lock().await;
        *slot = None;
        info!(remote = %remote.fmt_short(), "RouterClient usuniety");
    }

    /// Obsluguje pojedynczy bidi stream — odczytuje `ModelRequest`, dispatch,
    /// odsyla `ModelResponse` albo strumieniuje `ModelStreamChunk`.
    async fn handle_stream(
        mut send: iroh::endpoint::SendStream,
        mut recv: iroh::endpoint::RecvStream,
        transcript_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String, u64)>>>,
        command_tx: mpsc::UnboundedSender<MeetingCommand>,
    ) -> Result<()> {
        let request: ModelRequest = read_frame(&mut recv)
            .await
            .map_err(|e| anyhow::anyhow!("read ModelRequest: {e}"))?
            .ok_or_else(|| anyhow::anyhow!("peer zamknal stream bez requestu"))?;

        debug!(
            request_id = %request.request_id,
            stream = request.stream,
            "ModelRequest odebrany"
        );

        if request.stream {
            Self::handle_streaming_request(&request, &mut send, transcript_rx).await?;
        } else {
            let response = if let Some(cmd_response) =
                Self::try_handle_tool_command(&request, &command_tx).await
            {
                cmd_response
            } else {
                Self::process_request(&request)
            };

            write_frame(&mut send, &response)
                .await
                .map_err(|e| anyhow::anyhow!("write ModelResponse: {e}"))?;
            send.finish().ok();
        }
        Ok(())
    }

    /// Sprawdza czy request zawiera komende narzedzia w polu prompt.
    async fn try_handle_tool_command(
        request: &ModelRequest,
        command_tx: &mpsc::UnboundedSender<MeetingCommand>,
    ) -> Option<ModelResponse> {
        let payload = match &request.payload {
            ModelPayload::Completion(p) => p,
            _ => return None,
        };

        let prompt = payload.prompt.as_deref()?;
        let json: serde_json::Value = serde_json::from_str(prompt).ok()?;
        let tool = json.get("tool")?.as_str()?;

        let (response_tx, response_rx) = oneshot::channel();

        let cmd = match tool {
            "teams-bot.join_meeting" => {
                let meeting_url = json
                    .get("params")
                    .and_then(|p| p.get("meeting_url"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();

                if meeting_url.is_empty() {
                    return Some(error_resp(
                        &request.request_id,
                        ErrorType::InvalidRequest,
                        "Brak meeting_url w parametrach join_meeting",
                    ));
                }

                info!(meeting_url = %meeting_url, "Komenda: join_meeting");
                MeetingCommand::JoinMeeting {
                    meeting_url,
                    response_tx,
                }
            }
            "teams-bot.leave_meeting" => {
                info!("Komenda: leave_meeting");
                MeetingCommand::LeaveMeeting { response_tx }
            }
            "teams-bot.get_status" => {
                debug!("Komenda: get_status");
                MeetingCommand::GetStatus { response_tx }
            }
            _ => {
                warn!(tool = tool, "Nieznana komenda");
                return Some(error_resp(
                    &request.request_id,
                    ErrorType::InvalidRequest,
                    &format!("Nieznana komenda: {tool}"),
                ));
            }
        };

        if command_tx.send(cmd).is_err() {
            return Some(error_resp(
                &request.request_id,
                ErrorType::InternalError,
                "Kanal komend zamkniety",
            ));
        }

        let result_text = match tokio::time::timeout(std::time::Duration::from_secs(300), response_rx).await {
            Ok(Ok(text)) => text,
            Ok(Err(_)) => "Blad: kanal odpowiedzi zamkniety".to_string(),
            Err(_) => "Blad: timeout oczekiwania na wykonanie komendy".to_string(),
        };

        Some(ModelResponse {
            request_id: request.request_id.clone(),
            result: ModelResult::Completion(CompletionResult {
                text: result_text,
                reasoning_content: None,
                model: "meeting-bot".to_string(),
                finish_reason: Some("stop".to_string()),
                tool_calls: None,
                detected_intent: None,
                detected_tools: None,
                transcribed_text: None,
                speaker_id: None,
                speaker_name: None,
            }),
            metrics: None,
        })
    }

    /// Przetwarza jednorazowy ModelRequest.
    fn process_request(request: &ModelRequest) -> ModelResponse {
        match &request.payload {
            ModelPayload::Completion(payload) => {
                let response_text = format!(
                    "Meeting bot kontener — odebrano completion request: {} wiadomosci",
                    payload.messages.len()
                );
                ModelResponse {
                    request_id: request.request_id.clone(),
                    result: ModelResult::Completion(CompletionResult {
                        text: response_text,
                        reasoning_content: None,
                        model: "meeting-bot".to_string(),
                        finish_reason: Some("stop".to_string()),
                        tool_calls: None,
                        detected_intent: None,
                        detected_tools: None,
                        transcribed_text: None,
                        speaker_id: None,
                        speaker_name: None,
                    }),
                    metrics: None,
                }
            }
            ModelPayload::Audio(payload) => match &payload.operation {
                AudioOperation::STT { .. } => ModelResponse {
                    request_id: request.request_id.clone(),
                    result: ModelResult::Audio(AudioResult {
                        data: AudioResultData::Text(String::new()),
                        model: "meeting-bot-stt".to_string(),
                    }),
                    metrics: None,
                },
                AudioOperation::TTS { input, voice, .. } => {
                    debug!(text_len = input.len(), voice = %voice, "TTS request");
                    ModelResponse {
                        request_id: request.request_id.clone(),
                        result: ModelResult::Audio(AudioResult {
                            data: AudioResultData::Audio(Vec::new()),
                            model: "meeting-bot-tts".to_string(),
                        }),
                        metrics: None,
                    }
                }
                _ => error_resp(
                    &request.request_id,
                    ErrorType::InvalidRequest,
                    "Meeting bot nie obsluguje tej operacji audio",
                ),
            },
            _ => error_resp(
                &request.request_id,
                ErrorType::InvalidRequest,
                "Meeting bot nie obsluguje tego typu requestu",
            ),
        }
    }

    /// Streaming — wysyla chunki transkrypcji na zywo przez length-prefixed rkyv.
    async fn handle_streaming_request(
        request: &ModelRequest,
        send: &mut iroh::endpoint::SendStream,
        transcript_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String, u64)>>>,
    ) -> Result<()> {
        let request_id = &request.request_id;
        debug!(request_id = %request_id, "Streaming: start");

        let mut rx = transcript_rx.lock().await;
        while let Some((speaker, text, _timestamp_ms)) = rx.recv().await {
            let chunk_text = format!("[{}]: {}\n", speaker, text);
            let chunk = ModelStreamChunk {
                request_id: request_id.clone(),
                chunk: StreamChunkType::TextDelta(chunk_text),
            };
            write_frame(send, &chunk)
                .await
                .map_err(|e| anyhow::anyhow!("write chunk: {e}"))?;
        }

        let done_chunk = ModelStreamChunk {
            request_id: request_id.clone(),
            chunk: StreamChunkType::Done { final_metrics: None },
        };
        write_frame(send, &done_chunk)
            .await
            .map_err(|e| anyhow::anyhow!("write done chunk: {e}"))?;
        send.finish().ok();

        debug!(request_id = %request_id, "Streaming: zakonczony");
        Ok(())
    }
}

/// Laduje Ed25519 `SecretKey` z pliku albo generuje nowy (i zapisuje, jesli
/// `path` podana). Brak `path` = ephemeral — po restarcie nowy `EndpointId`.
/// Dekoduje Ed25519 secret key z 64-znakowego hex. Używany gdy env
/// `BOT_SECRET_KEY_HEX` jest ustawione (np. kontener odpalony z MeetingManagera).
fn load_secret_key_from_hex(hex: &str) -> Result<SecretKey> {
    let trimmed = hex.trim();
    if trimmed.len() != 64 {
        anyhow::bail!(
            "BOT_SECRET_KEY_HEX ma {} znakow, wymagane 64 (32 bajty hex)",
            trimmed.len()
        );
    }
    let bytes = hex::decode(trimmed).context("dekodowanie BOT_SECRET_KEY_HEX")?;
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    info!("Wczytano Ed25519 secret key z env BOT_SECRET_KEY_HEX");
    Ok(SecretKey::from_bytes(&arr))
}

fn load_or_generate_secret_key(path: Option<&str>) -> Result<SecretKey> {
    let Some(path) = path else {
        warn!("Brak secret_key_path — generuje ephemeral key");
        return Ok(SecretKey::generate());
    };

    let path = Path::new(path);
    if path.exists() {
        let bytes = std::fs::read(path)
            .with_context(|| format!("wczytanie klucza z {}", path.display()))?;
        if bytes.len() != 32 {
            anyhow::bail!(
                "plik klucza {} ma {} bajtow, wymagane 32",
                path.display(),
                bytes.len()
            );
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        info!(path = %path.display(), "wczytano Ed25519 secret key");
        return Ok(SecretKey::from_bytes(&arr));
    }

    let key = SecretKey::generate();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    std::fs::write(path, key.to_bytes())
        .with_context(|| format!("zapis klucza do {}", path.display()))?;
    info!(path = %path.display(), "wygenerowano i zapisano Ed25519 secret key");
    Ok(key)
}

fn error_resp(request_id: &str, error_type: ErrorType, message: &str) -> ModelResponse {
    ModelResponse {
        request_id: request_id.to_string(),
        result: ModelResult::Error(ErrorInfo {
            error_type,
            message: message.to_string(),
            details: None,
        }),
        metrics: None,
    }
}
